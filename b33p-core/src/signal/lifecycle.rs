//! Opening the data and running a session on it: the key, the store, linking and relinking.

use std::{
    cell::{Cell, RefCell},
    path::PathBuf,
    rc::Rc,
    sync::mpsc,
    time::Duration,
};

use futures::{channel::oneshot, future, pin_mut};
use presage::{
    Error as PresageError, Manager,
    libsignal_service::configuration::SignalServers,
    model::identity::OnNewIdentity,
    proto::sync_message,
    store::{ContentsStore, StateStore},
};
use presage_store_sqlite::SqliteStore;
use sqlx::{
    ConnectOptions,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous},
};
use tokio::{runtime::Runtime, task::LocalSet};
use tracing::{info, warn};

use crate::{
    Command, Error, Event, Events, Failure, HistoryTransfer, Password, Preferences, QrMatrix,
    attachments,
    local::LocalStore,
    outbox::{Sending, Sends as _},
    scrub::{scrub, scrubbed},
    transfer,
    vault::{self, KeySource, KeyringKey, NoKeyring},
};

use super::{
    Background, Ctx, GroupIds, Mgr,
    avatars::AvatarFetcher,
    commands::{command_failed, handle, outgoing, send_interrupted},
    conversations::{display_number, send_conversations, send_latest_messages, send_unread},
    history::sweep_expired,
    receive::{Repairs, start_receiving},
    send::request_sync,
    storage_sync::follow_storage,
    stories::{expire_stories, send_stories},
};

/// How an [`attempt`] ended, other than failing.
pub(super) enum Ending {
    /// The UI went away.
    Gone,
    /// The data here is to be deleted ([`Command::StartOver`], [`Command::Unlink`]); then
    /// everything starts again.
    Wipe,
}

/// Why an [`attempt`] failed: `message` is for the user, `detail` for the log.
#[derive(Debug)]
pub(super) struct Failed {
    pub(super) failure: Failure,
    pub(super) message: String,
    pub(super) detail: String,
}

impl Failed {
    fn new(failure: Failure, message: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            failure,
            message: message.into(),
            detail: detail.into(),
        }
    }

    /// Something unexpected, said as it is.
    pub(super) fn other(message: String) -> Self {
        Self::new(Failure::Other, message.clone(), message)
    }
}

impl From<String> for Failed {
    fn from(message: String) -> Self {
        Self::other(message)
    }
}

/// Opens the data and runs the session on it, from the password prompt or linking on.
/// Everything it started has stopped, and the database is closed, once it returns.
pub(super) fn attempt(
    device_name: &RefCell<String>,
    commands: &mut CommandRx,
    events: &Events,
    preferences: &Rc<Cell<Preferences>>,
    lock: &mut Option<std::fs::File>,
    use_password: bool,
) -> Result<Ending, Failed> {
    let paths = Paths::new()?;
    if lock.is_none() {
        *lock = Some(lock_data(&paths)?);
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    let local = tokio::task::LocalSet::new();
    let (result, closed) = match unlock(
        &runtime,
        &local,
        &paths,
        commands,
        events,
        preferences,
        device_name,
        use_password,
    ) {
        Ok(Unlocked::Open(store, tables, closed)) => (
            local.block_on(
                &runtime,
                session(
                    device_name,
                    store,
                    tables,
                    commands,
                    events.clone(),
                    preferences.clone(),
                    &paths,
                ),
            ),
            Some(closed),
        ),
        Ok(Unlocked::Gone) => (Ok(Ending::Gone), None),
        Ok(Unlocked::StartOver) => (Ok(Ending::Wipe), None),
        Err(failed) => (Err(failed), None),
    };
    // Drop the tasks still holding the store, then wait for its connections to close, so
    // `Backend::shutdown` returns only once the database is no longer in use (and it can be
    // opened again, or deleted). Inside the runtime: a task stopped mid-query drops a pooled
    // connection, which hands itself back to the pool on the runtime (outside it, sqlx panics
    // and the wait below never happens).
    {
        let _context = runtime.enter();
        drop(local);
    }
    drop(runtime);
    // Nothing is sent on `closed`: it disconnects once the database is closed.
    if let Some(closed) = closed
        && let Err(mpsc::RecvTimeoutError::Timeout) = closed.recv_timeout(Duration::from_secs(5))
    {
        warn!("the database did not close within 5 s");
    }
    result
}

/// The backend side of the command bridge (see [`run`]).
type CommandRx = tokio::sync::mpsc::UnboundedReceiver<Command>;

pub(super) enum AfterFailure {
    Retry,
    StartOver,
    UsePassword,
    Gone,
}

/// Waits for the user's answer to [`Event::Failed`]. A keyring that isn't there yet is tried
/// again by itself, sooner at first: at login it is often unlocked a moment after b33p starts.
pub(super) fn wait_after_failure(
    commands: &mut CommandRx,
    preferences: &Cell<Preferences>,
    device_name: &RefCell<String>,
    failure: Failure,
) -> AfterFailure {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("failed to build tokio runtime");
    let mut waits = [2, 5, 10, 30].into_iter();
    loop {
        let next = commands.recv();
        let command = if failure == Failure::KeyringUnavailable {
            let wait = Duration::from_secs(waits.next().unwrap_or(60));
            match runtime.block_on(tokio::time::timeout(wait, next)) {
                Ok(command) => command,
                Err(_) => return AfterFailure::Retry,
            }
        } else {
            runtime.block_on(next)
        };
        match command {
            None => return AfterFailure::Gone,
            Some(Command::Retry) => return AfterFailure::Retry,
            Some(Command::StartOver) if failure.can_start_over() => {
                return AfterFailure::StartOver;
            }
            Some(Command::UsePassword) if failure.can_use_password() => {
                return AfterFailure::UsePassword;
            }
            Some(Command::Preferences(changed)) => preferences.set(changed),
            Some(Command::SetDeviceName(name)) => *device_name.borrow_mut() = name,
            // Nothing else can be done meanwhile.
            Some(_) => {}
        }
    }
}

/// Disconnects once every connection to a store has been closed.
type Closed = mpsc::Receiver<()>;

pub(super) struct Paths {
    dir: PathBuf,
    db: PathBuf,
    key_source: PathBuf,
    /// Keyring entry holding the database key.
    keyring_account: String,
}

impl Paths {
    pub(super) fn new() -> Result<Self, String> {
        let dir = crate::settings::data_dir().ok_or("could not determine a data directory")?;
        // A separate data directory gets its own keyring entry, so it never replaces the
        // default one.
        let keyring_account = if crate::settings::custom_data_dir() {
            format!("{}:{}", vault::DEFAULT_ACCOUNT, dir.display())
        } else {
            vault::DEFAULT_ACCOUNT.to_owned()
        };
        // 0700: the database is only as private as the directory holding it.
        vault::restrict_dir(&dir)
            .map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
        // Earlier versions wrote it world-readable.
        match vault::restrict_file(&dir.join("b33p.key")) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                warn!(error = %scrubbed(&error), "could not make the key source private");
            }
            _ => {}
        }
        Ok(Self {
            db: dir.join("b33p.db3"),
            key_source: dir.join("b33p.key"),
            dir,
            keyring_account,
        })
    }

    /// Exists while a message history transfer is unfinished. Its one-time key lives only in
    /// memory, so if the app quits meanwhile the link is redone on the next start.
    fn transfer_marker(&self) -> PathBuf {
        self.dir.join("history-transfer.pending")
    }

    /// Locked by the b33p that has this data open.
    fn lock(&self) -> PathBuf {
        self.dir.join("b33p.lock")
    }
}

/// Keeps a second b33p away from the same data: two processes on one registration would race
/// each other's session state, and messages would fail to decrypt. The desktop's
/// single-instance check usually stops it sooner, but that can't be relied on (no session bus,
/// a binary started by hand on macOS). The lock goes with the process, however it ends.
fn lock_data(paths: &Paths) -> Result<std::fs::File, Failed> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(paths.lock())
        .map_err(|e| format!("failed to open {}: {e}", paths.lock().display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(Failed::new(
            Failure::AlreadyRunning,
            "b33p is already running. Quit the other one, then try again.",
            "the data directory is locked by another process",
        )),
        Err(std::fs::TryLockError::Error(error)) => {
            Err(format!("failed to lock {}: {error}", paths.lock().display()).into())
        }
    }
}

/// Deletes the messages and keys stored here, for linking again from scratch. The settings
/// (elsewhere) stay, and so does the lock this process holds.
pub(super) fn wipe(paths: &Paths) -> Result<(), String> {
    info!("deleting the local data");
    for path in [
        paths.db.clone(),
        paths.db.with_extension("db3-wal"),
        paths.db.with_extension("db3-shm"),
        paths.transfer_marker(),
    ] {
        match std::fs::remove_file(&path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(format!("Couldn't delete {}: {error}", path.display()));
            }
            _ => {}
        }
    }
    // The key last: while the database exists, it is the only way into it.
    if matches!(
        KeySource::read(&paths.key_source),
        Ok(Some(KeySource::Keyring))
    ) && let Err(error) = vault::delete_keyring_key(&paths.keyring_account)
    {
        // Harmless: a new key replaces it when linking again.
        warn!(error = %scrubbed(&error), "could not delete the database key from the keyring");
    }
    match std::fs::remove_file(&paths.key_source) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(format!(
            "Couldn't delete {}: {error}",
            paths.key_source.display()
        )),
        _ => Ok(()),
    }
}

/// What [`unlock`] came to, other than failing.
enum Unlocked {
    Open(SqliteStore, LocalStore, Closed),
    /// The UI quit while being asked for a password.
    Gone,
    /// The password was forgotten: [`Command::StartOver`] instead of one.
    StartOver,
}

/// Obtains the database key and opens the store. Keyring access blocks (D-Bus on Linux), so
/// this runs outside the runtime and only enters it to open the database. `use_password`
/// protects a new database with a password even if a keyring might come along.
#[allow(clippy::too_many_arguments)]
fn unlock(
    runtime: &Runtime,
    local: &LocalSet,
    paths: &Paths,
    commands: &mut CommandRx,
    events: &Events,
    preferences: &Cell<Preferences>,
    device_name: &RefCell<String>,
    use_password: bool,
) -> Result<Unlocked, Failed> {
    let source = KeySource::read(&paths.key_source).map_err(|e| {
        Failed::new(
            Failure::NoKeyRecorded,
            "The record of how your messages are protected is damaged, so they can't be opened.",
            e.to_string(),
        )
    })?;
    let (salt, kdf) = match source {
        Some(KeySource::Keyring) => {
            let key = match vault::keyring_key(&paths.keyring_account) {
                KeyringKey::Found(key) => key,
                KeyringKey::Missing => {
                    return Err(Failed::new(
                        Failure::KeyMissing,
                        "The key to your messages is missing from the system keyring, so they \
                         can't be opened. Start over to link this device again.",
                        "the database key is missing from the keyring",
                    ));
                }
                KeyringKey::Unavailable(reason) => {
                    return Err(Failed::new(
                        Failure::KeyringUnavailable,
                        "The system keyring, which holds the key to your messages, isn't \
                         available yet. Make sure it is running and unlocked; b33p keeps trying.",
                        reason,
                    ));
                }
            };
            return open_store(runtime, local, paths, &key)
                .map(|(store, tables, closed)| Unlocked::Open(store, tables, closed))
                .map_err(OpenError::failed);
        }
        Some(KeySource::Password { salt, kdf }) => (salt, kdf),
        None if paths.db.exists() => {
            return Err(Failed::new(
                Failure::NoKeyRecorded,
                "b33p can't tell how your messages were protected, so they can't be opened. \
                 Start over to link this device again.",
                "the database has no recorded key source",
            ));
        }
        None => {
            let created = if use_password {
                Err(NoKeyring::Absent("a password was chosen instead".into()))
            } else {
                vault::create_keyring_key(&paths.keyring_account)
            };
            match created {
                Ok(key) => {
                    write_source(paths, &KeySource::Keyring)?;
                    return open_store(runtime, local, paths, &key)
                        .map(|(store, tables, closed)| Unlocked::Open(store, tables, closed))
                        .map_err(OpenError::failed);
                }
                // Locked now, but it may be unlocked in a moment: a password is for good, so
                // that is the user's choice to make.
                Err(NoKeyring::Locked(reason)) => {
                    return Err(Failed::new(
                        Failure::KeyringLocked,
                        "The system keyring is locked. Unlock it to keep the key to your \
                         messages there, or protect them with a password instead.",
                        reason,
                    ));
                }
                Err(NoKeyring::Absent(reason)) => {
                    info!(reason = %scrubbed(&reason), "no usable system keyring; protecting the database with a password");
                    let salt = vault::random_salt();
                    let kdf = vault::Kdf::CURRENT;
                    write_source(paths, &KeySource::Password { salt, kdf })?;
                    (salt, kdf)
                }
            }
        }
    };

    // Until the database exists, the password is being chosen rather than checked.
    let new = !paths.db.exists();
    let mut error = None;
    loop {
        events.send(Event::PasswordRequired {
            new,
            error: error.take(),
        });
        let password = match wait_for_password(commands, preferences, device_name) {
            Answer::Password(password) => password,
            Answer::StartOver => return Ok(Unlocked::StartOver),
            Answer::Gone => return Ok(Unlocked::Gone),
        };
        // The UI checks this too, but the key is the only way back into the messages, so the
        // backend doesn't take a too-short one from any frontend.
        if new && let Err(problem) = check_new_password(password.expose()) {
            error = Some(problem);
            continue;
        }
        let key = vault::password_key(password.expose(), &salt, kdf);
        match open_store(runtime, local, paths, &key) {
            Ok((store, tables, closed)) => return Ok(Unlocked::Open(store, tables, closed)),
            Err(OpenError::WrongKey) if !new => {
                info!("wrong password for the database");
                error = Some("Wrong password.".into());
            }
            Err(error) => return Err(error.failed()),
        }
    }
}

fn write_source(paths: &Paths, source: &KeySource) -> Result<(), String> {
    source
        .write(&paths.key_source)
        .map_err(|e| format!("failed to write {}: {e}", paths.key_source.display()))
}

/// The policy for a new database password, the same one the password screen applies.
fn check_new_password(password: &str) -> Result<(), String> {
    crate::state::password_problem(password).map_or(Ok(()), Err)
}

/// What came instead of the password the backend waits for.
enum Answer {
    Password(Password),
    StartOver,
    Gone,
}

fn wait_for_password(
    commands: &mut CommandRx,
    preferences: &Cell<Preferences>,
    device_name: &RefCell<String>,
) -> Answer {
    // Nothing else can be done before the database is open, so other commands are dropped.
    loop {
        match commands.blocking_recv() {
            Some(Command::Unlock(password)) => return Answer::Password(password),
            Some(Command::StartOver) => return Answer::StartOver,
            Some(Command::Preferences(changed)) => preferences.set(changed),
            Some(Command::SetDeviceName(name)) => *device_name.borrow_mut() = name,
            Some(_) => {}
            None => return Answer::Gone,
        }
    }
}

/// Why [`open_store`] couldn't open the database.
#[derive(Debug)]
pub(super) enum OpenError {
    /// Not the key it was encrypted with (SQLCipher then reads garbage: "file is not a
    /// database").
    WrongKey,
    /// Written by a newer b33p: it has migrations this one doesn't know.
    TooNew,
    /// Its migrations differ from this build's, or one was left half-done.
    Incompatible(String),
    Other(String),
}

impl OpenError {
    fn of_store(error: &presage_store_sqlite::SqliteStoreError) -> Self {
        use presage_store_sqlite::SqliteStoreError;
        match error {
            SqliteStoreError::Db(error) => Self::of_db(error),
            SqliteStoreError::Migrate(error) => Self::of_migration(error),
            error => Self::Other(error.to_string()),
        }
    }

    fn of_db(error: &sqlx::Error) -> Self {
        // SQLITE_NOTADB, as sqlx gives SQLite's extended result codes.
        if let sqlx::Error::Database(e) = error
            && e.code().as_deref() == Some("26")
        {
            return Self::WrongKey;
        }
        Self::Other(error.to_string())
    }

    fn of_migration(error: &sqlx::migrate::MigrateError) -> Self {
        use sqlx::migrate::MigrateError;
        match error {
            // The first read of a wrong key may come from the migrator.
            MigrateError::Execute(e) | MigrateError::ExecuteMigration(e, _) => match Self::of_db(e)
            {
                Self::WrongKey => Self::WrongKey,
                _ => Self::Other(error.to_string()),
            },
            MigrateError::VersionMissing(_) => Self::TooNew,
            error @ (MigrateError::VersionMismatch(_)
            | MigrateError::VersionTooOld(..)
            | MigrateError::Dirty(_)) => Self::Incompatible(error.to_string()),
            error => Self::Other(error.to_string()),
        }
    }

    /// What the user is told. Never that the password is wrong unless it is: they might
    /// delete their data over it.
    fn failed(self) -> Failed {
        match self {
            Self::WrongKey => Failed::new(
                Failure::KeyMissing,
                "The key in the system keyring doesn't open your messages. Start over to link \
                 this device again.",
                "the database key doesn't open the database",
            ),
            Self::TooNew => Failed::new(
                Failure::StoreTooNew,
                "Your messages were saved by a newer version of b33p. Install that version \
                 again to open them.",
                "the database has migrations this build doesn't know",
            ),
            Self::Incompatible(detail) => Failed::new(
                Failure::StoreIncompatible,
                "Your messages were saved by a version of b33p that this one can't read. \
                 Install the version you used before to open them.",
                detail,
            ),
            Self::Other(detail) => Failed::new(
                Failure::Other,
                "Your messages couldn't be opened. Try again; if it keeps failing, the log \
                 says why.",
                detail,
            ),
        }
    }
}

fn open_store(
    runtime: &Runtime,
    local: &LocalSet,
    paths: &Paths,
    key: &vault::Key,
) -> Result<(SqliteStore, LocalStore, Closed), OpenError> {
    // The store doesn't expose its connection pool, so it can't be closed and awaited. Instead,
    // a never-used collation holds `open`: SQLite destroys a collation when its connection
    // closes (after closing the database file), and the pool's options hold the last copy.
    let (open, closed) = mpsc::channel::<()>();
    let options = SqliteConnectOptions::new()
        .filename(&paths.db)
        .create_if_missing(true)
        .foreign_keys(true)
        // The receive task and the command loop share the pool; without a timeout a write that
        // overlaps a read fails with SQLITE_BUSY instead of waiting. WAL lets readers and the
        // writer proceed together. FULL, not NORMAL: losing the last commits on power loss could
        // lose session state, and then messages can't be decrypted.
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .busy_timeout(Duration::from_secs(10))
        // sqlx keeps its own copy of this string; it can't be wiped.
        .pragma("key", vault::sqlcipher_pragma(key).to_string())
        // Overwrites what is deleted, which otherwise stays in free pages for anyone with the
        // passphrase. SQLCipher's build has it on; SQLite's default depends on the build
        // (<https://sqlite.org/pragma.html#pragma_secure_delete>).
        .pragma("secure_delete", "ON")
        // The key is part of the SQL text (pragmas take no parameters), so statements must
        // never reach the log, whatever RUST_LOG says.
        .disable_statement_logging()
        .collation("b33p_close_guard", move |a, b| {
            let _open = &open;
            a.cmp(b)
        });
    let store = local
        .block_on(
            runtime,
            SqliteStore::open_with_options(options.clone(), OnNewIdentity::Trust),
        )
        .map_err(|e| OpenError::of_store(&e))?;
    // Same options, so the same key and close guard.
    let tables = local
        .block_on(runtime, LocalStore::open(options))
        .map_err(|e| match e {
            crate::local::OpenError::Db(e) => OpenError::of_db(&e),
            crate::local::OpenError::TooNew(_) => OpenError::TooNew,
        })?;
    // Once presage's tables are there. Only for speed: without it everything still works.
    if let Err(error) = local.block_on(runtime, tables.index_messages()) {
        warn!(error = %scrubbed(&error), "could not index the stored messages");
    }
    // SQLite creates its files with the process umask, typically world-readable; another user
    // could copy them and attack the password offline. (The directory is private too.)
    for path in [
        paths.db.clone(),
        paths.db.with_extension("db3-wal"),
        paths.db.with_extension("db3-shm"),
    ] {
        if let Err(error) = vault::restrict_file(&path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!(path = %scrubbed(&path.display()), error = %scrubbed(&error), "could not make a database file private");
        }
    }
    Ok((store, tables, closed))
}

async fn session(
    device_name: &RefCell<String>,
    store: SqliteStore,
    tables: LocalStore,
    command_rx: &mut CommandRx,
    events: Events,
    preferences: Rc<Cell<Preferences>>,
    paths: &Paths,
) -> Result<Ending, Failed> {
    crate::client::identify();
    let mut linked = match Manager::load_registered(store.clone()).await {
        Ok(manager) => match Marker::read(paths) {
            None => {
                test_servers_unless_production(&manager.registration_data().signal_servers)?;
                let account = display_number(&manager.registration_data().phone_number);
                events.send(Event::Linked { account });
                Some((manager, Vec::new()))
            }
            Some(marker) => {
                unlink(manager).await;
                events.send(Event::Error(Error::unreachable(
                    "The message history transfer was interrupted. Scan the code to link this \
                     device again and retry.",
                )));
                reset(store.clone(), &tables, paths, marker.keeps()).await?;
                link_with_history(
                    store.clone(),
                    &tables,
                    device_name,
                    &events,
                    command_rx,
                    paths,
                    &preferences,
                    marker,
                )
                .await?
            }
        },
        Err(PresageError::NotYetRegisteredError) => {
            // Not registered, yet with chats: linking again failed earlier (and is being
            // tried again), so what is here must stay whatever becomes of the transfer.
            let mode = if has_contents(&store).await {
                Marker::Relink
            } else {
                Marker::Link
            };
            link_with_history(
                store.clone(),
                &tables,
                device_name,
                &events,
                command_rx,
                paths,
                &preferences,
                mode,
            )
            .await?
        }
        Err(error) => {
            return Err(Failed::new(
                Failure::Other,
                "Your account couldn't be read from the stored data. Try again; if it keeps \
                 failing, start over to link this device again.",
                format!("failed to load registration: {error}"),
            ));
        }
    };
    // Runs until the UI goes away; if Signal removes this device meanwhile, links it again.
    loop {
        // `None`: the UI went away while linking.
        let Some((manager, pending)) = linked else {
            return Ok(Ending::Gone);
        };
        match run_linked(
            manager,
            pending,
            &events,
            &tables,
            command_rx,
            &preferences,
            device_name,
        )
        .await
        {
            Ended::Gone => return Ok(Ending::Gone),
            Ended::Unlinked => return Ok(Ending::Wipe),
            Ended::Removed => {}
        }
        events.send(Event::Error(Error::refused(
            "This device was removed from your Signal account. Scan the code to link it again; \
             the messages here are kept.",
        )));
        // Linking clears the dead registration; the conversations stay.
        linked = link_with_history(
            store.clone(),
            &tables,
            device_name,
            &events,
            command_rx,
            paths,
            &preferences,
            Marker::Relink,
        )
        .await?;
    }
}

/// Whether the store holds contacts or groups, from an earlier link.
async fn has_contents(store: &SqliteStore) -> bool {
    let contacts = store
        .contacts()
        .await
        .is_ok_and(|mut contacts| contacts.next().is_some());
    contacts
        || store
            .groups()
            .await
            .is_ok_and(|mut groups| groups.next().is_some())
}

/// How [`run_linked`] ended.
enum Ended {
    /// The UI went away.
    Gone,
    /// Signal removed this device; it has to be linked again.
    Removed,
    /// [`Command::Unlink`]: removed from the account here; the data is to be deleted.
    Unlinked,
}

/// Runs a linked session: syncs, receives and handles commands, until the UI goes away, this
/// device is unlinked, or Signal removes it.
async fn run_linked(
    mut manager: Mgr,
    pending: Vec<Command>,
    events: &Events,
    tables: &LocalStore,
    command_rx: &mut CommandRx,
    preferences: &Rc<Cell<Preferences>>,
    device_name: &RefCell<String>,
) -> Ended {
    // b33p previews a link itself, and only with the user's `link_previews` on; presage would
    // otherwise fetch the page of every message's first link.
    manager.set_automatic_link_previews(false);
    // b33p shows stories: without this, the server drops every story sent to it, unread
    // (Signal-Server `WebSocketConnection`, `X-Signal-Receive-Stories`).
    manager.set_receive_stories(true);
    let storage_read = tables.storage_version().await.unwrap_or(0) > 0;
    let (outbox, queued) = tokio::sync::mpsc::channel(crate::outbox::CAPACITY);
    let sending = tokio::task::spawn_local(Sending::new(manager.clone()).event_loop(queued));
    let ctx = Ctx {
        threads: Rc::default(),
        events: events.clone(),
        tables: tables.clone(),
        preferences: preferences.clone(),
        expiring: Rc::default(),
        story_kept: Rc::default(),
        storage_changed: Rc::default(),
        storage_read: Rc::new(tokio::sync::watch::Sender::new(storage_read)),
        trust: Rc::default(),
        group_ids: GroupIds::default(),
        background: Background::default(),
        outbox,
    };
    let (threads, events) = (ctx.threads.clone(), ctx.events.clone());
    if send_conversations(&manager, &ctx).await == 0 {
        // Phones don't push their contacts to a newly linked device; they have to be asked.
        // The answer arrives through `receive` as `Received::Contacts`.
        info!("no contacts yet; requesting them from the primary device");
        if let Err(error) = ctx.outbox.request_contacts().await {
            warn!(error = %scrubbed(&error), "failed to request contacts");
        }
    }
    if !storage_read {
        // Just linked: who is blocked comes with the phone's settings, which take a while (and
        // the storage key to read them with may have been missed). Signal's apps ask for both.
        for kind in [
            sync_message::request::Type::Blocked,
            sync_message::request::Type::Keys,
        ] {
            request_sync(&manager, &ctx.outbox, kind).await;
        }
    }
    // Before receiving starts, or what arrives meanwhile would count twice (or not at all).
    send_unread(&manager, &ctx).await;
    send_interrupted(&manager, &ctx).await;

    let story_over = send_stories(&manager, &ctx).await;
    // In the background, so receiving starts right away.
    let latest = tokio::task::spawn_local(send_latest_messages(manager.clone(), ctx.clone()));
    let sweeping = tokio::task::spawn_local(sweep_expired(ctx.clone()));
    let following = tokio::task::spawn_local(follow_storage(manager.clone(), ctx.clone()));
    let stories =
        tokio::task::spawn_local(expire_stories(manager.clone(), ctx.clone(), story_over));

    let downloads = attachments::Downloads::new(manager.registration_data().signal_servers.clone());
    let avatars = AvatarFetcher::new(manager.clone(), threads.clone(), events.clone());
    avatars.request();
    let removed = Rc::new(tokio::sync::Notify::new());
    // Shared by every start of receiving: the limits it keeps must outlive a restart, and a
    // second avatar fetcher would send the first one's requests again, alongside.
    let repairs = Rc::new(RefCell::new(Repairs::default()));
    let mut receiving = start_receiving(
        &manager,
        &ctx,
        &removed,
        avatars.clone(),
        repairs.clone(),
        Duration::ZERO,
    );
    let mut failures = 0u32;

    // What the other side sees, one command at a time and in order; see `outgoing`. Their
    // sends then go through the outbox, with everyone else's.
    let (in_order, mut ordered) = tokio::sync::mpsc::unbounded_channel::<Command>();
    let ordering = tokio::task::spawn_local({
        let (mut manager, ctx, downloads) = (manager.clone(), ctx.clone(), downloads.clone());
        async move {
            while let Some(command) = ordered.recv().await {
                handle(&mut manager, &ctx, &downloads, command).await;
            }
        }
    });

    let mut pending = pending.into_iter();
    let ended = loop {
        let command = match pending.next() {
            Some(command) => command,
            None => tokio::select! {
                command = command_rx.recv() => match command {
                    Some(command) => command,
                    None => break Ended::Gone,
                },
                () = removed.notified() => break Ended::Removed,
                finished = &mut receiving => {
                    // `receive` ends on its own only once the device was removed.
                    let Err(error) = finished else { break Ended::Removed };
                    // Anything else it couldn't handle: start over, rather than look connected
                    // while nothing arrives; later and later should it keep failing.
                    warn!(error = %scrubbed(&error), "receiving failed");
                    events.send(Event::Error(Error::unreachable(
                        "Receiving messages failed; trying again.",
                    )));
                    failures += 1;
                    let after = Duration::from_secs(5 << failures.min(6));
                    receiving = start_receiving(
                        &manager,
                        &ctx,
                        &removed,
                        avatars.clone(),
                        repairs.clone(),
                        after,
                    );
                    continue;
                }
            },
        };
        if let Command::Unlink = command {
            info!("unlinking this device");
            receiving.abort();
            unlink(manager.clone()).await;
            break Ended::Unlinked;
        }
        if let Command::SetDeviceName(name) = command {
            // For the next link; the account keeps the name it was linked with.
            *device_name.borrow_mut() = name;
            continue;
        }
        if outgoing(&command) {
            // Kept from here until it went out: should b33p stop before, it shows as failed.
            if let Err(error) = ctx.tables.keep_unsent(&command).await {
                warn!(%error, "failed to keep a message until it is sent");
            }
            // It fails only if the task taking them in order panicked: then say the message
            // failed rather than show it as on its way. It is kept, so it shows as failed at
            // the next start too.
            if let Err(tokio::sync::mpsc::error::SendError(command)) = in_order.send(command) {
                if let Command::Send {
                    conversation,
                    timestamp,
                    ..
                } = &command
                {
                    events.send(Event::SendFailed {
                        conversation: conversation.clone(),
                        timestamp: *timestamp,
                        retryable: false,
                    });
                }
                command_failed(
                    &events,
                    Error::storage("the task sending messages in order has stopped"),
                    command,
                );
            }
            continue;
        }
        handle(&mut manager, &ctx, &downloads, command).await;
    };
    ctx.background.stop();
    ordering.abort();
    latest.abort();
    sweeping.abort();
    following.abort();
    stories.abort();
    receiving.abort();
    // Not drained: after an unlink there is nobody to send to, and a session after this one
    // must not send alongside it.
    sending.abort();
    ended
}

/// Links this device and, if the user chose so on the phone, transfers the message history.
/// The history has to be in place before live messages arrive, so this happens before
/// anything else. A failed transfer, or the phone asking for it, starts over with a new QR
/// code, so the user can simply try again. In between, a first link wipes the database, so
/// nothing of a half-done import remains; linking again (`Marker::Relink`) keeps the messages
/// that were already here.
///
/// Returns the commands that arrived during the transfer, or `None` if the UI went away.
#[allow(clippy::too_many_arguments)]
async fn link_with_history(
    store: SqliteStore,
    tables: &LocalStore,
    device_name: &RefCell<String>,
    events: &Events,
    commands: &mut CommandRx,
    paths: &Paths,
    preferences: &Cell<Preferences>,
    mode: Marker,
) -> Result<Option<(Mgr, Vec<Command>)>, Failed> {
    loop {
        // Nothing can be done before linking, so commands are dropped until then; a new name
        // needs a new code. `true` if renamed, `false` once the UI has gone away.
        let renamed = async {
            while let Some(command) = commands.recv().await {
                match command {
                    Command::Preferences(changed) => preferences.set(changed),
                    Command::SetDeviceName(name) => {
                        *device_name.borrow_mut() = name;
                        return true;
                    }
                    _ => {}
                }
            }
            false
        };
        let name = device_name.borrow().clone();
        let (manager, key) = tokio::select! {
            linked = link(store.clone(), name, events) => linked?,
            renamed = renamed => {
                if renamed {
                    continue;
                }
                return Ok(None);
            }
        };
        let account = display_number(&manager.registration_data().phone_number);
        events.send(Event::Linked { account });
        let Some(key) = key else {
            return Ok(Some((manager, Vec::new())));
        };

        mode.write(paths)?;
        let mut pending = Vec::new();
        let result = {
            let transfer = transfer::receive_history(&manager, key, &paths.dir, tables, events);
            pin_mut!(transfer);
            loop {
                tokio::select! {
                    result = &mut transfer => break result,
                    command = commands.recv() => match command {
                        Some(Command::SkipHistory) => {
                            info!("skipping the message history transfer");
                            break Ok(transfer::Outcome::WithoutHistory);
                        }
                        Some(command) => pending.push(command),
                        // The marker makes the next start redo the link.
                        None => return Ok(None),
                    },
                }
            }
        };
        events.send(Event::HistoryTransfer(HistoryTransfer::Finished));
        match result {
            Ok(transfer::Outcome::Imported | transfer::Outcome::WithoutHistory) => {
                Marker::remove(paths)?;
                return Ok(Some((manager, pending)));
            }
            Ok(transfer::Outcome::Relink) => info!("the phone asked to link again"),
            Err(error) => {
                warn!(error = %scrub(&error.message), kind = ?error.kind, "message history transfer failed");
                events.send(Event::Error(Error::new(
                    error.kind,
                    "The message history couldn't be transferred. Scan the new code to link \
                     this device again and retry.",
                )));
            }
        }
        unlink(manager).await;
        reset(store.clone(), tables, paths, mode.keeps()).await?;
    }
}

/// Removes this device from the account before linking again, so the phone's list of linked
/// devices doesn't fill up with dead entries. Best effort: linking again works regardless.
async fn unlink(manager: Mgr) {
    if let Err(error) = manager.unlink_self().await {
        warn!(error = %scrubbed(&error), "failed to remove this device from the account");
    }
}

/// The file that says a history transfer is under way (see [`Paths::transfer_marker`]), and
/// whether it came with a first link or with linking again: then the messages that were here
/// before must survive whatever happens to the transfer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Marker {
    Link,
    Relink,
}

impl Marker {
    fn read(paths: &Paths) -> Option<Self> {
        let text = std::fs::read_to_string(paths.transfer_marker()).ok()?;
        // Empty: written by 0.1, which only knew first links.
        Some(if text.trim() == "relink" {
            Self::Relink
        } else {
            Self::Link
        })
    }

    fn write(self, paths: &Paths) -> Result<(), String> {
        let text = match self {
            Self::Link => "link\n",
            Self::Relink => "relink\n",
        };
        vault::write_private(&paths.transfer_marker(), text.as_bytes())
            .map_err(|e| format!("failed to write {}: {e}", paths.transfer_marker().display()))
    }

    fn remove(paths: &Paths) -> Result<(), String> {
        match std::fs::remove_file(paths.transfer_marker()) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(format!(
                "failed to remove {}: {error}",
                paths.transfer_marker().display()
            )),
            _ => Ok(()),
        }
    }

    /// What resetting for another try leaves in place.
    fn keeps(self) -> Keep {
        match self {
            Self::Link => Keep::Nothing,
            Self::Relink => Keep::Contents,
        }
    }
}

/// What [`reset`] leaves in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Keep {
    /// The messages, contacts and groups (and b33p's own records of them): linking again.
    Contents,
    /// Nothing: a first link, whose half-done import must go.
    Nothing,
}

/// Forgets the registration, and with `Keep::Nothing` everything stored with it, ready to link
/// again.
async fn reset(
    mut store: SqliteStore,
    tables: &LocalStore,
    paths: &Paths,
    keep: Keep,
) -> Result<(), String> {
    store
        .clear_registration()
        .await
        .map_err(|e| format!("failed to reset the database: {e}"))?;
    if keep == Keep::Nothing {
        store
            .clear_contents()
            .await
            .map_err(|e| format!("failed to reset the database: {e}"))?;
        tables
            .clear()
            .await
            .map_err(|e| format!("failed to reset the database: {e}"))?;
    }
    Marker::remove(paths)
}

// Other servers are a build's choice, not the environment's: `debug_assertions` alone is a
// profile setting a release can turn on.
#[cfg(all(feature = "test-servers", not(debug_assertions)))]
compile_error!("the `test-servers` feature is for test builds: build without `--release`");

/// The servers a new link goes to: Signal's.
#[cfg(not(feature = "test-servers"))]
fn link_servers() -> Result<SignalServers, String> {
    Ok(SignalServers::Production)
}

/// The servers a new link goes to. `B33P_SIGNAL_SERVERS` names others: `staging`, or a TOML
/// file describing servers of one's own, such as the testbox's, in the form presage-cli's
/// `--servers-file` reads.
#[cfg(feature = "test-servers")]
fn link_servers() -> Result<SignalServers, String> {
    let Ok(name) = std::env::var("B33P_SIGNAL_SERVERS") else {
        return Ok(SignalServers::Production);
    };
    if let Ok(servers) = name.parse() {
        return Ok(servers);
    }
    let text = std::fs::read_to_string(&name)
        .map_err(|error| format!("B33P_SIGNAL_SERVERS {name}: {error}"))?;
    let configuration: presage::libsignal_service::configuration::ServiceConfiguration =
        toml::from_str(&text).map_err(|error| format!("B33P_SIGNAL_SERVERS {name}: {error}"))?;
    Ok(SignalServers::Custom(Box::new(configuration)))
}

/// A store linked to other servers ([`link_servers`]) stays out of builds without them.
fn test_servers_unless_production(servers: &SignalServers) -> Result<(), String> {
    if cfg!(feature = "test-servers") || *servers == SignalServers::Production {
        Ok(())
    } else {
        Err(format!(
            "This store is linked to the {servers} servers, which only test builds use."
        ))
    }
}

/// Links this device; also returns the one-time key for the history transfer if the user
/// chose to transfer it.
async fn link(
    store: SqliteStore,
    device_name: String,
    events: &Events,
) -> Result<(Mgr, Option<[u8; 32]>), Failed> {
    let servers = link_servers()?;
    crate::client::identify();
    let (url_tx, url_rx) = oneshot::channel();
    let (linked, _) = future::join(
        Manager::link_secondary_device(store, servers, device_name, url_tx),
        async {
            if let Ok(mut url) = url_rx.await {
                let url = url
                    .query_pairs_mut()
                    .append_pair("capabilities", transfer::LINK_CAPABILITY)
                    .finish();
                // Said rather than swallowed: otherwise the user faces a blank panel and a link
                // that never completes.
                events.send(match QrMatrix::encode(url.as_str()) {
                    Some(qr) => Event::LinkQr(qr),
                    None => Event::Error(Error::refused(
                        "The linking code couldn't be drawn. Restart b33p to try again.",
                    )),
                });
            }
        },
    )
    .await;
    let manager = linked.map_err(|e| {
        Failed::new(
            Failure::LinkFailed,
            "Linking this device didn't work. Check the connection and try again.",
            format!("linking failed: {e}"),
        )
    })?;
    let history_key = manager.registration_data().ephemeral_backup_key();
    Ok((manager, history_key))
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::signal::conversations::conversation_id;
    use crate::signal::history::{stored_at, stored_since};
    use crate::signal::tests::aci;
    use crate::{ConversationId, MessageRef};
    use presage::{
        libsignal_service::{
            content::{Content, ContentBody, DataMessage},
            protocol::ServiceId,
        },
        store::Thread,
    };

    /// Counted in characters: seven accented letters are too short even though they take
    /// fourteen bytes, and eight are enough.
    #[test]
    fn new_passwords_need_eight_characters_not_bytes() {
        assert!(check_new_password("").is_err());
        assert!(check_new_password("äöüäöüä").is_err());
        assert!(check_new_password("äöüäöüäö").is_ok());
    }

    #[test]
    fn database_opens_only_with_its_key() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let paths = Paths {
            db: dir.join("test.db3"),
            key_source: dir.join("test.key"),
            dir: dir.clone(),
            keyring_account: "unused".into(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = LocalSet::new();

        let key = vault::password_key("correct horse", &[3; 16], vault::Kdf::V1);
        drop(open_store(&runtime, &local, &paths, &key).expect("creates the database"));
        let wrong = vault::password_key("wrong horse", &[3; 16], vault::Kdf::V1);
        // Told apart from every other failure: only this one may say "Wrong password."
        assert!(matches!(
            open_store(&runtime, &local, &paths, &wrong),
            Err(OpenError::WrongKey)
        ));
        assert!(open_store(&runtime, &local, &paths, &key).is_ok());
        // Encrypted: no plaintext SQLite header.
        let bytes = std::fs::read(&paths.db).unwrap();
        assert!(!bytes.starts_with(b"SQLite format 3"));
    }

    #[test]
    fn erased_rows_stay_neither_in_the_log_nor_in_free_pages() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let key = vault::password_key("correct horse", &[3; 16], vault::Kdf::V1);
        let log = paths.db.with_extension("db3-wal");
        open_and_close(&paths, &key, async |_, tables| {
            let secure_delete: i64 = sqlx::query_scalar("PRAGMA secure_delete")
                .fetch_one(tables.pool())
                .await
                .unwrap();
            assert_eq!(secure_delete, 1);
            let conversation = ConversationId("a".into());
            let message = MessageRef {
                author_id: "alice".into(),
                timestamp: 10,
            };
            tables
                .record_edit(&conversation, &message, 20, "secret")
                .await
                .unwrap();
            tables.forget(&[(conversation, message)]).await.unwrap();
            assert_ne!(std::fs::metadata(&log).unwrap().len(), 0);
            assert!(tables.checkpoint().await.unwrap());
            assert_eq!(std::fs::metadata(&log).unwrap().len(), 0);
        })
        .unwrap();
    }

    pub(crate) fn test_paths(dir: &std::path::Path) -> Paths {
        Paths {
            db: dir.join("b33p.db3"),
            key_source: dir.join("b33p.key"),
            dir: dir.to_path_buf(),
            keyring_account: "unused".into(),
        }
    }

    /// Opens and fully closes the database, as `attempt` does between tries.
    pub(crate) fn open_and_close(
        paths: &Paths,
        key: &vault::Key,
        then: impl AsyncFnOnce(&SqliteStore, &LocalStore),
    ) -> Result<(), OpenError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = LocalSet::new();
        let (store, tables, closed) = open_store(&runtime, &local, paths, key)?;
        local.block_on(&runtime, then(&store, &tables));
        drop((store, tables));
        {
            let _context = runtime.enter();
            drop(local);
        }
        drop(runtime);
        assert_eq!(
            closed.recv_timeout(Duration::from_secs(5)),
            Err(mpsc::RecvTimeoutError::Disconnected),
            "the database did not close"
        );
        Ok(())
    }

    /// A database from a newer b33p isn't taken for a wrong password, which would invite its
    /// owner to delete it.
    #[test]
    fn a_database_from_a_newer_version_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let key = vault::password_key("correct horse", &[3; 16], vault::Kdf::V1);
        open_and_close(&paths, &key, async |_, tables| {
            // As a later migration would leave it.
            sqlx::query(
                "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
                 VALUES (99991231000000, 'from the future', 1, x'00', 0)",
            )
            .execute(tables.pool())
            .await
            .unwrap();
        })
        .unwrap();
        let reopened = open_and_close(&paths, &key, async |_, _| {});
        assert!(matches!(reopened, Err(OpenError::TooNew)), "{reopened:?}");
        let failed = reopened.unwrap_err().failed();
        assert_eq!(failed.failure, Failure::StoreTooNew);
        assert!(!failed.message.to_lowercase().contains("password"));
    }

    /// Linking again after the device was removed keeps the messages, whatever becomes of the
    /// history transfer; a first link starts from nothing each time.
    #[test]
    fn resetting_for_another_link_keeps_what_linking_again_needs() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let key = vault::password_key("correct horse", &[3; 16], vault::Kdf::V1);
        let ann = ConversationId::contact("ann");
        let kept = async |store: &SqliteStore, tables: &LocalStore, keep: Keep| {
            tables.update(&ann, |s| s.pinned = Some(1)).await.unwrap();
            Marker::Relink.write(&paths).unwrap();
            reset(store.clone(), tables, &paths, keep).await.unwrap();
            assert!(Marker::read(&paths).is_none(), "the marker goes");
            tables.settings(&ann).await.unwrap().is_some()
        };
        open_and_close(&paths, &key, async |store, tables| {
            assert!(kept(store, tables, Keep::Contents).await);
            assert!(!kept(store, tables, Keep::Nothing).await);
        })
        .unwrap();
    }

    /// Linking again that failed and is tried again keeps the chats: the store tells.
    #[test]
    fn a_store_with_contacts_is_linked_again_not_anew() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let key = vault::password_key("correct horse", &[3; 16], vault::Kdf::V1);
        open_and_close(&paths, &key, async |store, _| {
            assert!(!has_contents(store).await);
            let mut store = store.clone();
            store
                .save_contact(&presage::model::contacts::Contact {
                    uuid: presage::libsignal_service::prelude::Uuid::from_bytes([7; 16]),
                    phone_number: None,
                    name: "Ann".into(),
                    verified: Default::default(),
                    profile_key: Vec::new(),
                    expire_timer: 0,
                    expire_timer_version: 2,
                    inbox_position: 0,
                    avatar: None,
                })
                .await
                .unwrap();
            assert!(has_contents(&store).await);
        })
        .unwrap();
    }

    #[test]
    fn the_marker_remembers_whether_it_was_a_first_link() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        assert_eq!(Marker::read(&paths), None);
        for marker in [Marker::Link, Marker::Relink] {
            marker.write(&paths).unwrap();
            assert_eq!(Marker::read(&paths), Some(marker));
        }
        // As 0.1 left it.
        std::fs::write(paths.transfer_marker(), b"").unwrap();
        assert_eq!(Marker::read(&paths), Some(Marker::Link));
        assert_eq!(Marker::Link.keeps(), Keep::Nothing);
        assert_eq!(Marker::Relink.keeps(), Keep::Contents);
        Marker::remove(&paths).unwrap();
        Marker::remove(&paths).unwrap();
        assert_eq!(Marker::read(&paths), None);
    }

    #[test]
    fn wiping_deletes_the_data_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let key = vault::password_key("correct horse", &[3; 16], vault::Kdf::V1);
        KeySource::Password {
            salt: [3; 16],
            kdf: vault::Kdf::V1,
        }
        .write(&paths.key_source)
        .unwrap();
        open_and_close(&paths, &key, async |_, _| {}).unwrap();
        Marker::Link.write(&paths).unwrap();
        let lock = lock_data(&paths).unwrap();
        std::fs::write(tmp.path().join("unrelated"), b"").unwrap();
        wipe(&paths).unwrap();
        let mut left: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["b33p.lock", "unrelated"]);
        // Again, with nothing there: done.
        wipe(&paths).unwrap();
        drop(lock);
    }

    #[test]
    fn only_one_process_has_the_data_open() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let held = lock_data(&paths).unwrap();
        let second = lock_data(&paths);
        assert!(
            matches!(&second, Err(failed) if failed.failure == Failure::AlreadyRunning),
            "{second:?}"
        );
        drop(held);
        assert!(lock_data(&paths).is_ok());
    }

    /// A message from `sender` sent at `ts`, to store.
    fn stored(sender: ServiceId, ts: i64) -> Content {
        use presage::libsignal_service::{content::Metadata, push_service::DEFAULT_DEVICE_ID};
        let at = chrono::DateTime::from_timestamp_millis(ts).unwrap();
        Content {
            metadata: Metadata {
                sender,
                destination: sender,
                sender_device: *DEFAULT_DEVICE_ID,
                pni_verified: None,
                client_timestamp: at,
                server_timestamp: at,
                needs_receipt: false,
                unidentified_sender: false,
                was_plaintext: false,
                server_guid: None,
            },
            body: ContentBody::DataMessage(DataMessage {
                body: Some(format!("at {ts}")),
                timestamp: Some(ts as u64),
                ..Default::default()
            }),
        }
    }

    /// What presage asks for one chat's messages is found through b33p's index on its table,
    /// rather than by walking every chat's messages by time (`messages_before`) or the whole
    /// table (`messages`). b33p reads a chat's newest message, its pages of history, what is
    /// unread, recent safety number notes, media and search hits with `messages_before`.
    #[test]
    fn a_chats_messages_are_found_through_its_index() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let key = vault::password_key("correct horse", &[3; 16], vault::Kdf::V1);
        open_and_close(&paths, &key, async |_, tables| {
            use sqlx::Row as _;
            // As presage's `messages_before` and `messages` have them (content.rs).
            const COLUMNS: &str = "ts, server_ts, sender_service_id, sender_device_id,
                destination_service_id, needs_receipt, unidentified_sender, content_body,
                was_plaintext, pni_verified";
            const THREAD: &str = "thread_id = (
                SELECT id FROM threads WHERE group_master_key = ? OR recipient_id = ?)";
            let before = format!(
                "EXPLAIN QUERY PLAN SELECT {COLUMNS} FROM thread_messages
                 WHERE {THREAD} AND ts < ? ORDER BY ts DESC LIMIT ?"
            );
            let range = format!(
                "EXPLAIN QUERY PLAN SELECT {COLUMNS} FROM thread_messages
                 WHERE {THREAD} AND coalesce(ts > ?, ts >= ?, true)
                    AND coalesce(ts < ?, ts <= ?, true)
                 ORDER BY ts DESC"
            );
            // One connection that has seen the index: EXPLAIN doesn't check whether the schema
            // changed since the connection last read it, as statements that run do.
            let mut connection = tables.pool().acquire().await.unwrap();
            sqlx::query("SELECT count(*) FROM thread_messages")
                .fetch_one(&mut *connection)
                .await
                .unwrap();
            let mut plan = async |sql: &str, binds: usize| -> Vec<String> {
                let mut query = sqlx::query(sqlx::AssertSqlSafe(sql.to_owned()))
                    .bind(None::<Vec<u8>>)
                    .bind(vec![7u8; 16]);
                for _ in 0..binds {
                    query = query.bind(Some(5i64));
                }
                query
                    .fetch_all(&mut *connection)
                    .await
                    .unwrap()
                    .iter()
                    .map(|row| row.get::<String, _>(3))
                    .collect()
            };
            let before = plan(&before, 2).await;
            assert!(
                before.iter().any(|step| step
                    .contains("INDEX b33p_thread_messages_thread_ts (thread_id=? AND ts<?)")),
                "{before:?}"
            );
            let range = plan(&range, 4).await;
            assert!(
                range
                    .iter()
                    .any(|step| step.contains("INDEX b33p_thread_messages_thread_ts (thread_id=?)")),
                "{range:?}"
            );
            // In the index's order already.
            for step in before.iter().chain(&range) {
                assert!(!step.contains("TEMP B-TREE"), "{step}");
                assert!(!step.starts_with("SCAN thread_messages"), "{step}");
            }
        })
        .unwrap();
    }

    /// Disappearing messages go from presage's store and b33p's tables at once, from their
    /// own thread only, by contact or group.
    #[test]
    fn expired_messages_go_in_one_transaction() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let key = vault::password_key("correct horse", &[3; 16], vault::Kdf::V1);
        open_and_close(&paths, &key, async |store, tables| {
            let ann = ServiceId::Aci(aci(1));
            let (chat, group) = (Thread::Contact(ann), Thread::Group([9; 32]));
            for ts in [10, 20] {
                store.save_message(&chat, stored(ann, ts)).await.unwrap();
                store.save_message(&group, stored(ann, ts)).await.unwrap();
            }
            let message = |ts| MessageRef {
                author_id: ann.service_id_string(),
                timestamp: ts,
            };
            let entry =
                |thread: &Thread, ts| (conversation_id(thread), message(ts), Some(thread.clone()));
            let chat_id = conversation_id(&chat);
            tables
                .set_expiries(&chat_id, &[(message(10), 1), (message(20), 1)])
                .await
                .unwrap();
            tables
                .delete_messages(&[entry(&chat, 10), entry(&group, 20)])
                .await
                .unwrap();
            let kept =
                async |thread: &Thread, ts| store.message(thread, ts).await.unwrap().is_some();
            assert!(!kept(&chat, 10).await);
            assert!(kept(&chat, 20).await);
            assert!(kept(&group, 10).await);
            assert!(!kept(&group, 20).await);
            let (due, _) = tables.due(5).await.unwrap();
            assert_eq!(due, [(chat_id, message(20))]);
        })
        .unwrap();
    }

    /// What a thread has from some time on comes from its newest messages, over several pages
    /// and none from other threads.
    #[test]
    fn recent_contents_come_a_page_at_a_time() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let key = vault::password_key("correct horse", &[3; 16], vault::Kdf::V1);
        open_and_close(&paths, &key, async |store, _| {
            let who = |n: u8| {
                ServiceId::parse_from_service_id_string(&format!(
                    "00000000-0000-0000-0000-0000000000{n:02}"
                ))
                .unwrap()
            };
            let (ann, bob) = (Thread::Contact(who(1)), Thread::Contact(who(2)));
            for ts in 1..=100 {
                store
                    .save_message(&ann, stored(who(1), ts * 2))
                    .await
                    .unwrap();
                store
                    .save_message(&bob, stored(who(2), ts * 2 + 1))
                    .await
                    .unwrap();
            }
            let times = async |thread: &Thread, from: u64| -> Vec<u64> {
                stored_since(store, thread, from)
                    .await
                    .unwrap()
                    .iter()
                    .map(stored_at)
                    .collect()
            };
            let expected: Vec<u64> = (21..=100).rev().map(|ts| ts * 2).collect();
            assert_eq!(times(&ann, 41).await, expected);
            assert_eq!(times(&ann, 42).await, expected);
            assert_eq!(times(&ann, 201).await, Vec::<u64>::new());
            assert_eq!(times(&ann, 200).await, [200]);
            assert_eq!(times(&ann, 0).await.len(), 100);
            assert_eq!(times(&bob, 199).await, [201, 199]);
            let nobody = Thread::Contact(who(3));
            assert_eq!(times(&nobody, 0).await, Vec::<u64>::new());
        })
        .unwrap();
    }
}
