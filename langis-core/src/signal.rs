//! Real backend: presage on a single-threaded tokio runtime (presage's futures are `!Send`).

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use futures::{StreamExt, channel::oneshot, future, pin_mut};
use presage::{
    Error as PresageError, Manager,
    libsignal_service::{
        configuration::SignalServers,
        content::{Content, ContentBody, DataMessage, GroupContextV2},
        protocol::ServiceId,
        push_service::ServiceError,
    },
    manager::Registered,
    model::{identity::OnNewIdentity, messages::Received},
    proto::{
        SyncMessage,
        sync_message::{Content as SyncContent, Sent},
    },
    store::{ContentExt, ContentsStore, StateStore, Thread},
};
use presage_store_sqlite::SqliteStore;
use sqlx::{
    ConnectOptions,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous},
};
use tokio::{runtime::Runtime, task::LocalSet};
use tracing::{debug, info, warn};

use crate::{
    ChatMessage, Command, Config, Conversation, ConversationId, Event, Events, HistoryTransfer,
    Inbox, MessageRef, Password, Picture, QrMatrix, Quote, Reaction, apply_reaction, attachments,
    now_millis, transfer,
    vault::{self, KeySource, KeyringKey},
};

pub(crate) type Mgr = Manager<SqliteStore, Registered>;

/// Maps the opaque ids handed to the UI back to presage threads.
type Threads = Rc<RefCell<HashMap<ConversationId, Thread>>>;

pub fn run(config: Config, commands: Inbox, events: Events) {
    attachments::clean_opened();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    let local = tokio::task::LocalSet::new();
    let (result, closed) = match unlock(&runtime, &local, &commands, &events) {
        Ok(Some((store, closed))) => (
            local.block_on(&runtime, session(config, store, commands, events.clone())),
            Some(closed),
        ),
        // The UI went away while we were waiting for a password.
        Ok(None) => (Ok(()), None),
        Err(error) => (Err(error), None),
    };
    if let Err(error) = result {
        let _ = events.send(Event::Error(error));
    }
    // Drop the tasks still holding the store, then wait for its connections to close, so
    // `Backend::shutdown` returns only once the database is no longer in use. Inside the
    // runtime: a task stopped mid-query drops a pooled connection, which hands itself back to
    // the pool on the runtime (outside it, sqlx panics and the wait below never happens).
    {
        let _context = runtime.enter();
        drop(local);
    }
    drop(runtime);
    if let Some(closed) = closed {
        let _ = closed.recv_timeout(Duration::from_secs(5));
    }
}

/// Disconnects once every connection to a store has been closed.
type Closed = mpsc::Receiver<()>;

struct Paths {
    dir: PathBuf,
    db: PathBuf,
    key_source: PathBuf,
    /// Keyring entry holding the database key.
    keyring_account: String,
}

impl Paths {
    fn new() -> Result<Self, String> {
        // `LANGIS_DATA_DIR` keeps a separate account, e.g. to link a second device for testing.
        // Its database key gets its own keyring entry, so it never replaces the default one.
        // A development aid: release builds always use the standard place.
        let custom = if cfg!(debug_assertions) {
            std::env::var_os("LANGIS_DATA_DIR")
        } else {
            None
        };
        let (dir, keyring_account) = match custom {
            Some(dir) => {
                let dir = PathBuf::from(dir);
                let account = format!("{}:{}", vault::DEFAULT_ACCOUNT, dir.display());
                (dir, account)
            }
            None => (
                directories::ProjectDirs::from("org", "langis", "langis")
                    .ok_or("could not determine a data directory")?
                    .data_dir()
                    .to_path_buf(),
                vault::DEFAULT_ACCOUNT.to_owned(),
            ),
        };
        // 0700: the database is only as private as the directory holding it.
        vault::restrict_dir(&dir)
            .map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
        // Earlier versions wrote it world-readable.
        match vault::restrict_file(&dir.join("langis.key")) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                warn!(%error, "could not make the key source private");
            }
            _ => {}
        }
        Ok(Self {
            db: dir.join("langis.db3"),
            key_source: dir.join("langis.key"),
            dir,
            keyring_account,
        })
    }

    /// Exists while a message history transfer is unfinished. Its one-time key lives only in
    /// memory, so if the app quits meanwhile the link is redone on the next start.
    fn transfer_marker(&self) -> PathBuf {
        self.dir.join("history-transfer.pending")
    }
}

/// Obtains the database key and opens the store; `None` if the UI quit while being asked for
/// a password. Keyring access blocks (D-Bus on Linux), so this runs outside the runtime and
/// only enters it to open the database.
fn unlock(
    runtime: &Runtime,
    local: &LocalSet,
    commands: &Inbox,
    events: &Events,
) -> Result<Option<(SqliteStore, Closed)>, String> {
    let paths = Paths::new()?;
    let (salt, kdf) = match KeySource::read(&paths.key_source).map_err(|e| e.to_string())? {
        Some(KeySource::Keyring) => {
            let key = match vault::keyring_key(&paths.keyring_account) {
                KeyringKey::Found(key) => key,
                KeyringKey::Missing => {
                    return Err(format!(
                        "The database key is missing from the system keyring. To link this device \
                         again, delete {}.",
                        paths.dir.display()
                    ));
                }
                KeyringKey::Unavailable(reason) => {
                    return Err(format!(
                        "The system keyring is unavailable ({reason}). Make sure it is running and unlocked."
                    ));
                }
            };
            return open_store(runtime, local, &paths, &key).map(Some);
        }
        Some(KeySource::Password { salt, kdf }) => (salt, kdf),
        None if paths.db.exists() => {
            return Err(format!(
                "{} has no recorded encryption key. To link this device again, delete it.",
                paths.db.display()
            ));
        }
        None => match vault::create_keyring_key(&paths.keyring_account) {
            Ok(key) => {
                write_source(&paths, &KeySource::Keyring)?;
                return open_store(runtime, local, &paths, &key).map(Some);
            }
            Err(reason) => {
                info!(%reason, "no usable system keyring; protecting the database with a password");
                let salt = vault::random_salt();
                let kdf = vault::Kdf::CURRENT;
                write_source(&paths, &KeySource::Password { salt, kdf })?;
                (salt, kdf)
            }
        },
    };

    // Until the database exists, the password is being chosen rather than checked.
    let new = !paths.db.exists();
    let mut error = None;
    loop {
        let _ = events.send(Event::PasswordRequired {
            new,
            error: error.take(),
        });
        let Some(password) = wait_for_password(commands) else {
            return Ok(None);
        };
        // The UI checks this too, but the key is the only way back into the messages, so the
        // backend doesn't take a too-short one from any frontend.
        if new && let Err(problem) = check_new_password(password.expose()) {
            error = Some(problem);
            continue;
        }
        let key = vault::password_key(password.expose(), &salt, kdf);
        match open_store(runtime, local, &paths, &key) {
            Ok(store) => return Ok(Some(store)),
            Err(reason) if !new => {
                warn!(%reason, "failed to unlock the database");
                error = Some("Wrong password.".into());
            }
            Err(reason) => return Err(reason),
        }
    }
}

fn write_source(paths: &Paths, source: &KeySource) -> Result<(), String> {
    source
        .write(&paths.key_source)
        .map_err(|e| format!("failed to write {}: {e}", paths.key_source.display()))
}

/// The policy for a new database password; mirrors `state::ChatState::unlock`.
fn check_new_password(password: &str) -> Result<(), String> {
    if password.chars().count() < crate::state::MIN_PASSWORD_LEN {
        return Err(format!(
            "Use at least {} characters.",
            crate::state::MIN_PASSWORD_LEN
        ));
    }
    Ok(())
}

fn wait_for_password(commands: &Inbox) -> Option<Password> {
    // Nothing else can be done before the database is open, so other commands are dropped.
    commands.iter().find_map(|command| match command {
        Command::Unlock(password) => Some(password),
        _ => None,
    })
}

fn open_store(
    runtime: &Runtime,
    local: &LocalSet,
    paths: &Paths,
    key: &vault::Key,
) -> Result<(SqliteStore, Closed), String> {
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
        // The key is part of the SQL text (pragmas take no parameters), so statements must
        // never reach the log, whatever RUST_LOG says.
        .disable_statement_logging()
        .collation("langis_close_guard", move |a, b| {
            let _ = &open;
            a.cmp(b)
        });
    let store = local
        .block_on(
            runtime,
            SqliteStore::open_with_options(options, OnNewIdentity::Trust),
        )
        .map_err(|e| format!("failed to open the database: {e}"))?;
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
            warn!(path = %path.display(), %error, "could not make a database file private");
        }
    }
    Ok((store, closed))
}

async fn session(
    config: Config,
    store: SqliteStore,
    commands: Inbox,
    events: Events,
) -> Result<(), String> {
    // crossbeam -> tokio bridge, so the UI side can use plain blocking channels. It ends on
    // shutdown, which closes `command_rx`.
    let (command_tx, mut command_rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Some(command) = commands.recv() {
            if command_tx.send(command).is_err() {
                break;
            }
        }
    });

    let paths = Paths::new()?;
    let mut linked = match Manager::load_registered(store.clone()).await {
        Ok(manager) if !paths.transfer_marker().exists() => {
            let account = display_number(&manager.registration_data().phone_number);
            let _ = events.send(Event::Linked { account });
            Some((manager, Vec::new()))
        }
        Ok(manager) => {
            unlink(manager).await;
            let _ = events.send(Event::Error(
                "The message history transfer was interrupted. Link this device again to retry."
                    .into(),
            ));
            reset(store.clone(), &paths).await?;
            link_with_history(store.clone(), &config, &events, &mut command_rx, &paths).await?
        }
        Err(PresageError::NotYetRegisteredError) => {
            link_with_history(store.clone(), &config, &events, &mut command_rx, &paths).await?
        }
        Err(error) => return Err(format!("failed to load registration: {error}")),
    };
    // Runs until the UI goes away; if Signal removes this device meanwhile, links it again.
    loop {
        // `None`: the UI went away while linking.
        let Some((manager, pending)) = linked else {
            return Ok(());
        };
        if !run_linked(manager, pending, &events, &mut command_rx).await {
            return Ok(());
        }
        let _ = events.send(Event::Error(
            "This device was removed from your Signal account, so it can't send or receive any more. \
             Scan the code to link it again; the messages here are kept."
                .into(),
        ));
        // Linking clears the dead registration; the conversations stay.
        linked =
            link_with_history(store.clone(), &config, &events, &mut command_rx, &paths).await?;
    }
}

/// Runs a linked session: syncs, receives and handles commands. Returns `false` once the UI
/// has gone away, `true` if Signal removed this device and it has to be linked again.
async fn run_linked(
    mut manager: Mgr,
    pending: Vec<Command>,
    events: &Events,
    command_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
) -> bool {
    let events = events.clone();
    let threads: Threads = Rc::default();
    if send_conversations(&manager, &threads, &events).await == 0 {
        // Phones don't push their contacts to a newly linked device; they have to be asked.
        // The answer arrives through `receive` as `Received::Contacts`.
        info!("no contacts yet; requesting them from the primary device");
        if let Err(error) = manager.request_contacts().await {
            warn!(%error, "failed to request contacts");
        }
    }

    // In the background, so receiving starts right away.
    let latest = tokio::task::spawn_local(send_latest_messages(
        manager.clone(),
        threads.clone(),
        events.clone(),
    ));

    let downloads = attachments::download_limit();
    let avatars = AvatarFetcher::new(manager.clone(), threads.clone(), events.clone());
    avatars.request();
    let removed = Rc::new(tokio::sync::Notify::new());
    let receiving = tokio::task::spawn_local(receive(
        manager.clone(),
        threads.clone(),
        events.clone(),
        avatars,
        removed.clone(),
    ));

    for command in pending {
        if let Err(error) = handle(&mut manager, &threads, &events, &downloads, command).await {
            let _ = events.send(Event::Error(error));
        }
    }
    let was_removed = loop {
        tokio::select! {
            command = command_rx.recv() => match command {
                Some(command) => {
                    if let Err(error) = handle(&mut manager, &threads, &events, &downloads, command).await {
                        let _ = events.send(Event::Error(error));
                    }
                }
                None => break false,
            },
            () = removed.notified() => break true,
        }
    };
    latest.abort();
    receiving.abort();
    was_removed
}

/// Whether Signal refuses this device's credentials: it was removed from the account (the
/// server answers 403, or 401), so retrying can't help.
fn removed_from_account(error: &PresageError<presage_store_sqlite::SqliteStoreError>) -> bool {
    use presage::libsignal_service::push_service::ServiceError;
    use reqwest_websocket::{Error as WsError, HandshakeError};
    match error {
        PresageError::ServiceError(ServiceError::Unauthorized) => true,
        PresageError::ServiceError(ServiceError::WsError(error)) => matches!(
            &**error,
            WsError::Handshake(HandshakeError::UnexpectedStatusCode(status)) if matches!(status.as_u16(), 401 | 403)
        ),
        _ => false,
    }
}

/// Links this device and, if the user chose so on the phone, transfers the message history.
/// The history has to be in place before live messages arrive, so this happens before
/// anything else. A failed transfer, or the phone asking for it, starts over with a new QR
/// code, so the user can simply try again; the database is wiped in between so nothing of a
/// half-done import remains.
///
/// Returns the commands that arrived during the transfer, or `None` if the UI went away.
async fn link_with_history(
    store: SqliteStore,
    config: &Config,
    events: &Events,
    commands: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    paths: &Paths,
) -> Result<Option<(Mgr, Vec<Command>)>, String> {
    loop {
        // Nothing can be done before linking, so commands are dropped until then.
        let ui_closed = async { while commands.recv().await.is_some() {} };
        let (manager, key) = tokio::select! {
            linked = link(store.clone(), config, events) => linked?,
            () = ui_closed => return Ok(None),
        };
        let account = display_number(&manager.registration_data().phone_number);
        let _ = events.send(Event::Linked { account });
        let Some(key) = key else {
            return Ok(Some((manager, Vec::new())));
        };

        write_marker(paths)?;
        let mut pending = Vec::new();
        let result = {
            let transfer = transfer::receive_history(&manager, key, &paths.dir, events);
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
        let _ = events.send(Event::HistoryTransfer(HistoryTransfer::Finished));
        match result {
            Ok(transfer::Outcome::Imported | transfer::Outcome::WithoutHistory) => {
                std::fs::remove_file(paths.transfer_marker()).map_err(|e| {
                    format!(
                        "failed to remove {}: {e}",
                        paths.transfer_marker().display()
                    )
                })?;
                return Ok(Some((manager, pending)));
            }
            Ok(transfer::Outcome::Relink) => info!("the phone asked to link again"),
            Err(error) => {
                warn!(%error, "message history transfer failed");
                let _ = events.send(Event::Error(format!(
                    "{error} Link this device again to retry."
                )));
            }
        }
        unlink(manager).await;
        reset(store.clone(), paths).await?;
    }
}

/// Removes this device from the account before linking again, so the phone's list of linked
/// devices doesn't fill up with dead entries. Best effort: linking again works regardless.
async fn unlink(manager: Mgr) {
    if let Err(error) = manager.unlink_self().await {
        warn!(%error, "failed to remove this device from the account");
    }
}

fn write_marker(paths: &Paths) -> Result<(), String> {
    std::fs::write(paths.transfer_marker(), b"")
        .map_err(|e| format!("failed to write {}: {e}", paths.transfer_marker().display()))
}

/// Forgets the registration and everything stored with it, ready to link again.
async fn reset(mut store: SqliteStore, paths: &Paths) -> Result<(), String> {
    store
        .clear_registration()
        .await
        .map_err(|e| format!("failed to reset the database: {e}"))?;
    store
        .clear_contents()
        .await
        .map_err(|e| format!("failed to reset the database: {e}"))?;
    match std::fs::remove_file(paths.transfer_marker()) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(format!(
            "failed to remove {}: {error}",
            paths.transfer_marker().display()
        )),
        _ => Ok(()),
    }
}

/// Links this device; also returns the one-time key for the history transfer if the user
/// chose to transfer it.
async fn link(
    store: SqliteStore,
    config: &Config,
    events: &Events,
) -> Result<(Mgr, Option<[u8; 32]>), String> {
    let (url_tx, url_rx) = oneshot::channel();
    let (linked, _) = future::join(
        Manager::link_secondary_device_with_history(
            store,
            SignalServers::Production,
            config.device_name.clone(),
            url_tx,
        ),
        async {
            if let Ok(mut url) = url_rx.await {
                let url = url
                    .query_pairs_mut()
                    .append_pair("capabilities", transfer::LINK_CAPABILITY)
                    .finish();
                // Said rather than swallowed: otherwise the user faces a blank panel and a link
                // that never completes.
                let _ = events.send(match QrMatrix::encode(url.as_str()) {
                    Some(qr) => Event::LinkQr(qr),
                    None => Event::Error(
                        "The linking code couldn't be drawn. Restart langis to try again.".into(),
                    ),
                });
            }
        },
    )
    .await;
    linked.map_err(|e| format!("linking failed: {e}"))
}

fn conversation_id(thread: &Thread) -> ConversationId {
    ConversationId(match thread {
        Thread::Contact(id) => format!("c:{}", id.service_id_string()),
        Thread::Group(key) => format!("g:{}", hex::encode(key)),
    })
}

/// Remembers how to reach a conversation and describes it for the UI.
fn register(threads: &Threads, thread: Thread, title: String, subtitle: String) -> Conversation {
    let id = conversation_id(&thread);
    let is_group = matches!(thread, Thread::Group(_));
    threads.borrow_mut().insert(id.clone(), thread);
    let title = if title.is_empty() {
        "Unknown".into()
    } else {
        title
    };
    Conversation {
        id,
        title,
        is_group,
        subtitle,
    }
}

/// A phone number as people write it: "+49 151 23456789".
fn display_number(
    number: &presage::libsignal_service::prelude::phonenumber::PhoneNumber,
) -> String {
    use presage::libsignal_service::prelude::phonenumber::Mode;
    number.format().mode(Mode::International).to_string()
}

fn members(count: usize) -> String {
    match count {
        1 => "1 member".into(),
        n => format!("{n} members"),
    }
}

/// The line under a conversation's title; see [`Conversation::subtitle`].
async fn subtitle(manager: &Mgr, thread: &Thread) -> String {
    match thread {
        Thread::Contact(id) if *id == own_id(manager) => "Messages to yourself".into(),
        Thread::Contact(id) => match manager.store().contact_by_id(id).await {
            Ok(Some(contact)) => contact
                .phone_number
                .as_ref()
                .map(display_number)
                .unwrap_or_default(),
            _ => String::new(),
        },
        Thread::Group(key) => match manager.store().group(*key).await {
            Ok(Some(group)) => members(group.members.len()),
            _ => String::new(),
        },
    }
}

/// Sends all synced contacts and groups; the UI merges them with what it already knows.
/// Returns the number of contacts.
async fn send_conversations(manager: &Mgr, threads: &Threads, events: &Events) -> usize {
    let store = manager.store();
    let mut all = Vec::new();
    if let Ok(contacts) = store.contacts().await {
        for contact in contacts.flatten() {
            let thread = Thread::Contact(ServiceId::Aci(contact.uuid.into()));
            let number = contact
                .phone_number
                .as_ref()
                .map(display_number)
                .unwrap_or_default();
            let title = if contact.name.is_empty() {
                number.clone()
            } else {
                contact.name
            };
            all.push(register(threads, thread, title, number));
        }
    }
    let contacts = all.len();
    // Messages to yourself; listed once there are some.
    all.push(register(
        threads,
        Thread::Contact(own_id(manager)),
        "Note to Self".into(),
        "Messages to yourself".into(),
    ));
    if let Ok(groups) = store.groups().await {
        for (key, group) in groups.flatten() {
            let count = members(group.members.len());
            all.push(register(threads, Thread::Group(key), group.title, count));
        }
    }
    let _ = events.send(Event::Conversations(all));
    contacts
}

/// Sends each conversation's newest stored message, so the UI can order conversations by
/// activity and show previews before any history is opened.
///
/// The store can only return whole time ranges, so this looks at the last week first and only
/// reaches further back for quiet conversations, instead of reading every chat's full history.
async fn send_latest_messages(manager: Mgr, threads: Threads, events: Events) {
    const DAY: u64 = 24 * 60 * 60 * 1000;
    let threads: Vec<_> = threads
        .borrow()
        .iter()
        .map(|(id, thread)| (id.clone(), thread.clone()))
        .collect();
    let mut names = Names::new();
    let now = now_millis();
    'threads: for (id, thread) in threads {
        for since in [
            now.saturating_sub(7 * DAY),
            now.saturating_sub(180 * DAY),
            0,
        ] {
            let Ok(contents) = manager.store().messages(&thread, since..).await else {
                continue 'threads;
            };
            // Newest first; skip receipts and the like.
            for content in contents.flatten() {
                if let Some(message) =
                    to_chat_message(&manager, &mut names, id.clone(), &content).await
                {
                    if !events.send(Event::Latest(message)) {
                        return;
                    }
                    continue 'threads;
                }
            }
        }
    }
}

/// Side of decoded avatars: a 40 px circle at up to 3x scaling.
const AVATAR_SIDE: u32 = 128;

/// Runs [`fetch_avatars`] one pass at a time: asking during a pass schedules one more after it,
/// instead of a second pass sending the same requests alongside.
#[derive(Clone)]
struct AvatarFetcher {
    run: Rc<Cell<AvatarRun>>,
    manager: Mgr,
    threads: Threads,
    events: Events,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AvatarRun {
    Idle,
    Running,
    Again,
}

impl AvatarFetcher {
    fn new(manager: Mgr, threads: Threads, events: Events) -> Self {
        Self {
            run: Rc::new(Cell::new(AvatarRun::Idle)),
            manager,
            threads,
            events,
        }
    }

    fn request(&self) {
        match self.run.get() {
            AvatarRun::Idle => {
                self.run.set(AvatarRun::Running);
                let this = self.clone();
                tokio::task::spawn_local(async move {
                    loop {
                        fetch_avatars(
                            this.manager.clone(),
                            this.threads.clone(),
                            this.events.clone(),
                        )
                        .await;
                        if this.run.get() == AvatarRun::Again {
                            this.run.set(AvatarRun::Running);
                        } else {
                            this.run.set(AvatarRun::Idle);
                            break;
                        }
                    }
                });
            }
            AvatarRun::Running => self.run.set(AvatarRun::Again),
            AvatarRun::Again => {}
        }
    }
}

/// Sends each conversation's profile or group picture. presage caches them in the store, so
/// only the first run downloads them, one at a time to stay clear of rate limits.
///
/// A profile picture needs the contact's profile key, which only arrives with their messages
/// (the contact sync no longer carries it). Until then the picture from the contact sync, i.e.
/// what the phone shows, stands in.
async fn fetch_avatars(mut manager: Mgr, threads: Threads, events: Events) {
    let threads: Vec<_> = threads
        .borrow()
        .iter()
        .map(|(id, thread)| (id.clone(), thread.clone()))
        .collect();
    let (mut found, mut none, mut failed) = (0, 0, 0);
    for (id, thread) in threads {
        let picture = match thread {
            Thread::Contact(ServiceId::Aci(aci)) => {
                let id = ServiceId::Aci(aci);
                let profile = match manager.store().profile_key(&id).await {
                    Ok(Some(key)) => {
                        manager
                            .retrieve_profile_avatar_by_uuid(aci.into(), key)
                            .await
                    }
                    _ => Ok(None),
                };
                match profile {
                    Ok(None) => {
                        let contact = manager.store().contact_by_id(&id).await.ok().flatten();
                        Ok(contact
                            .and_then(|c| c.avatar)
                            .map(|avatar| avatar.reader.to_vec()))
                    }
                    profile => profile,
                }
            }
            Thread::Contact(_) => continue,
            Thread::Group(key) => {
                // presage only refetches the group if this revision is newer than the cached one.
                let revision = manager
                    .store()
                    .group(key)
                    .await
                    .ok()
                    .flatten()
                    .map_or(0, |group| group.revision);
                let context = GroupContextV2 {
                    master_key: Some(key.to_vec()),
                    revision: Some(revision),
                    ..Default::default()
                };
                manager.retrieve_group_avatar(context).await
            }
        };
        match picture {
            Ok(Some(bytes)) => match decode_avatar(&bytes) {
                Some(avatar) => {
                    found += 1;
                    if !events.send(Event::Avatar {
                        conversation: id,
                        avatar: Arc::new(avatar),
                    }) {
                        return;
                    }
                }
                None => {
                    failed += 1;
                    debug!(conversation = id.0, "could not decode avatar");
                }
            },
            Ok(None) => none += 1,
            Err(PresageError::ServiceError(ServiceError::RateLimitExceeded { .. })) => {
                warn!("rate limited while fetching avatars; the rest are fetched next time");
                return;
            }
            Err(error) => {
                failed += 1;
                debug!(conversation = id.0, %error, "failed to fetch avatar");
            }
        }
    }
    debug!(found, none, failed, "fetched avatars");
}

/// Crops to a square and scales to [`AVATAR_SIDE`].
fn decode_avatar(bytes: &[u8]) -> Option<Picture> {
    let image = attachments::decode_limited(bytes)?;
    let square = image.resize_to_fill(
        AVATAR_SIDE,
        AVATAR_SIDE,
        image::imageops::FilterType::Lanczos3,
    );
    Some(Picture {
        width: AVATAR_SIDE,
        height: AVATAR_SIDE,
        rgba: square.into_rgba8().into_raw(),
    })
}

/// Sends a data message to a 1:1 chat or a group. presage fills in the message's timestamp
/// from `timestamp`.
async fn send_data(
    manager: &mut Mgr,
    thread: &Thread,
    mut message: DataMessage,
    timestamp: u64,
) -> Result<(), String> {
    let sent = match thread {
        Thread::Contact(recipient) => manager.send_message(*recipient, message, timestamp).await,
        Thread::Group(key) => {
            // Recipients compare this with their group state to spot stale context.
            let revision = match manager.store().group(*key).await {
                Ok(Some(group)) => group.revision,
                _ => 0,
            };
            message.group_v2 = Some(GroupContextV2 {
                master_key: Some(key.to_vec()),
                revision: Some(revision),
                ..Default::default()
            });
            manager.send_message_to_group(key, message, timestamp).await
        }
    };
    sent.map_err(|e| format!("failed to send: {e}"))
}

fn own_id(manager: &Mgr) -> ServiceId {
    ServiceId::Aci(manager.registration_data().service_ids.aci())
}

/// When we last reset the session with each sender; see `SESSION_RESET_INTERVAL`.
type Resets = HashMap<ServiceId, Instant>;

const SESSION_RESET_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Display names by sender service ID, so each sender is looked up in the store only once.
type Names = HashMap<String, String>;

async fn sender_name(manager: &Mgr, names: &mut Names, sender: ServiceId) -> String {
    let id = sender.service_id_string();
    if let Some(name) = names.get(&id) {
        return name.clone();
    }
    let name = manager
        .thread_title(&Thread::Contact(sender))
        .await
        .unwrap_or_default();
    names.insert(id, name.clone());
    name
}

/// Receives messages for the rest of the session, reconnecting with exponential backoff
/// whenever the connection drops (sleep, network changes, server timeouts).
/// Notifies `removed` (and stops) if Signal no longer accepts this device.
async fn receive(
    mut manager: Mgr,
    threads: Threads,
    events: Events,
    avatars: AvatarFetcher,
    removed: Rc<tokio::sync::Notify>,
) {
    const MAX_BACKOFF: Duration = Duration::from_secs(60);
    let mut names = Names::new();
    let mut resets = Resets::new();
    let mut backoff = Duration::from_secs(1);
    loop {
        match manager.receive_messages().await {
            Ok(messages) => {
                pin_mut!(messages);
                while let Some(received) = messages.next().await {
                    // Something came through, so the connection works again.
                    backoff = Duration::from_secs(1);
                    if !handle_received(
                        &manager,
                        &threads,
                        &events,
                        &avatars,
                        &mut names,
                        &mut resets,
                        received,
                    )
                    .await
                    {
                        return;
                    }
                }
                info!("connection to Signal closed");
            }
            Err(error) if removed_from_account(&error) => {
                warn!(%error, "Signal no longer accepts this device");
                removed.notify_one();
                return;
            }
            Err(error) => warn!(%error, "failed to connect to Signal"),
        }
        if !events.send(Event::Reconnecting) {
            return;
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// Returns `false` once the UI has gone away.
async fn handle_received(
    manager: &Mgr,
    threads: &Threads,
    events: &Events,
    avatars: &AvatarFetcher,
    names: &mut Names,
    resets: &mut Resets,
    received: Received,
) -> bool {
    match received {
        Received::QueueEmpty => events.send(Event::Synced),
        Received::Contacts => {
            names.clear();
            send_conversations(manager, threads, events).await;
            // A newly linked device first learns its contacts here.
            avatars.request();
            true
        }
        Received::DecryptionError(sender) => {
            warn!(sender = %sender.service_id_string(), "decryption error");
            // A broken session stays broken until someone resets it; the sender's app then
            // starts a new one (and can resend). Not more than hourly, so a peer that keeps
            // failing can't keep us resetting.
            let reset = matches!(sender, ServiceId::Aci(_))
                && resets
                    .get(&sender)
                    .is_none_or(|last| last.elapsed() > SESSION_RESET_INTERVAL);
            if reset {
                resets.insert(sender, Instant::now());
                match manager
                    .clone()
                    .send_session_reset(&sender, now_millis())
                    .await
                {
                    Ok(()) => info!(sender = %sender.service_id_string(), "reset the session"),
                    Err(error) => warn!(%error, "failed to reset the session"),
                }
            }
            // The envelope doesn't say whether it was a group message, so it goes in the 1:1 chat.
            let conversation =
                ensure_conversation(manager, threads, events, Thread::Contact(sender)).await;
            let name = sender_name(manager, names, sender).await;
            let body = if reset {
                format!(
                    "⚠ A message from {name} couldn't be decrypted. The connection was reset; it may arrive again."
                )
            } else {
                format!("⚠ A message from {name} couldn't be decrypted.")
            };
            events.send(Event::Message(ChatMessage {
                conversation,
                sender_id: sender.service_id_string(),
                body,
                sender: name,
                timestamp: now_millis(),
                ..Default::default()
            }))
        }
        Received::Content(content) => {
            let Ok(thread) = Thread::try_from(&*content) else {
                return true;
            };
            let id = ensure_conversation(manager, threads, events, thread).await;
            if let Some((target, reaction, remove)) = to_reaction(manager, names, &content).await {
                return events.send(Event::Reaction {
                    conversation: id,
                    target,
                    reaction,
                    remove,
                });
            }
            match to_chat_message(manager, names, id, &content).await {
                Some(message) => events.send(Event::Message(message)),
                None => true,
            }
        }
    }
}

/// Registers `thread` and announces it to the UI if it's the first we hear of it.
async fn ensure_conversation(
    manager: &Mgr,
    threads: &Threads,
    events: &Events,
    thread: Thread,
) -> ConversationId {
    let id = conversation_id(&thread);
    if !threads.borrow().contains_key(&id) {
        let title = manager.thread_title(&thread).await.unwrap_or_default();
        let subtitle = subtitle(manager, &thread).await;
        let conversation = register(threads, thread, title, subtitle);
        info!(title = conversation.title, "new conversation");
        events.send(Event::Conversations(vec![conversation]));
    }
    id
}

/// Extracts displayable text; returns `None` for receipts, typing indicators, etc.
/// `text` with Signal's mention placeholders replaced by "@Name".
async fn with_mentions(
    manager: &Mgr,
    names: &mut Names,
    text: &str,
    ranges: &[presage::proto::BodyRange],
) -> String {
    use presage::proto::body_range::AssociatedValue;
    let mut mentions = Vec::new();
    for range in ranges {
        let who = match &range.associated_value {
            Some(AssociatedValue::MentionAci(aci)) => ServiceId::parse_from_service_id_string(aci),
            Some(AssociatedValue::MentionAciBinary(bytes)) => {
                ServiceId::parse_from_service_id_binary(bytes)
            }
            _ => None,
        };
        if let Some(who) = who {
            let name = if who == own_id(manager) {
                "You".to_string()
            } else {
                sender_name(manager, names, who).await
            };
            mentions.push((range.start(), range.length(), name));
        }
    }
    crate::resolve_mentions(text, &mentions)
}

/// A reaction, from someone else or (synced from the phone) from us.
async fn to_reaction(
    manager: &Mgr,
    names: &mut Names,
    content: &Content,
) -> Option<(MessageRef, Reaction, bool)> {
    let (data, outgoing) = match &content.body {
        ContentBody::DataMessage(data) => (data, false),
        ContentBody::SynchronizeMessage(SyncMessage {
            content:
                Some(SyncContent::Sent(Sent {
                    message: Some(data),
                    ..
                })),
            ..
        }) => (data, true),
        _ => return None,
    };
    let reaction = data.reaction.as_ref()?;
    let target = MessageRef {
        author_id: reaction.target_author_aci.clone()?,
        timestamp: reaction.target_sent_timestamp?,
    };
    let sender = if outgoing {
        "You".to_string()
    } else {
        sender_name(manager, names, content.metadata.sender).await
    };
    let reaction_view = Reaction {
        emoji: reaction.emoji.clone().unwrap_or_default(),
        sender_id: content.metadata.sender.service_id_string(),
        sender,
        outgoing,
    };
    Some((target, reaction_view, reaction.remove()))
}

async fn to_chat_message(
    manager: &Mgr,
    names: &mut Names,
    conversation: ConversationId,
    content: &Content,
) -> Option<ChatMessage> {
    let (data, outgoing) = match &content.body {
        ContentBody::DataMessage(data) => (data, false),
        ContentBody::SynchronizeMessage(SyncMessage {
            content:
                Some(SyncContent::Sent(Sent {
                    message: Some(data),
                    ..
                })),
            ..
        }) => (data, true),
        _ => return None,
    };
    let body = with_mentions(
        manager,
        names,
        data.body.as_deref().unwrap_or_default(),
        &data.body_ranges,
    )
    .await;
    let attachments = attachments::of_message(data);
    if body.is_empty() && attachments.is_empty() {
        return None;
    }
    let sender = if outgoing {
        "You".to_string()
    } else {
        sender_name(manager, names, content.metadata.sender).await
    };
    let quote = match &data.quote {
        Some(quote) => {
            let author = match quote
                .author_aci
                .as_deref()
                .and_then(ServiceId::parse_from_service_id_string)
            {
                Some(author) if author == own_id(manager) => "You".to_string(),
                Some(author) => sender_name(manager, names, author).await,
                None => String::new(),
            };
            Some(Quote {
                author,
                text: with_mentions(
                    manager,
                    names,
                    quote.text.as_deref().unwrap_or_default(),
                    &quote.body_ranges,
                )
                .await,
                author_id: quote.author_aci.clone().unwrap_or_default(),
                timestamp: quote.id.unwrap_or_default(),
            })
        }
        None => None,
    };
    Some(ChatMessage {
        conversation,
        sender_id: content.metadata.sender.service_id_string(),
        sender,
        body,
        timestamp: content.timestamp(),
        outgoing,
        quote,
        attachments,
        reactions: Vec::new(),
    })
}

async fn handle(
    manager: &mut Mgr,
    threads: &Threads,
    events: &Events,
    downloads: &Rc<tokio::sync::Semaphore>,
    command: Command,
) -> Result<(), String> {
    let servers = manager.registration_data().signal_servers;
    match command {
        // In the background, so commands keep flowing while files download.
        Command::FetchAttachment(attachment) => {
            tokio::task::spawn_local(attachments::fetch(
                servers,
                attachment,
                events.clone(),
                downloads.clone(),
            ));
        }
        Command::SaveAttachment(attachment) => {
            tokio::task::spawn_local(attachments::save(
                servers,
                attachment,
                events.clone(),
                downloads.clone(),
            ));
        }
        // Already unlocked.
        Command::Unlock(_) => {}
        // Only meaningful during the transfer; see `link_with_history`.
        Command::SkipHistory => {}
        Command::LoadHistory(id) => {
            let Some(thread) = threads.borrow().get(&id).cloned() else {
                return Ok(());
            };
            let contents = manager
                .store()
                .messages(&thread, ..)
                .await
                .map_err(|e| format!("failed to load history: {e}"))?;
            let mut names = Names::new();
            let mut messages = Vec::new();
            // Oldest first, so reactions find their message and later ones win.
            let contents: Vec<_> = contents.flatten().collect();
            for content in contents.iter().rev() {
                if let Some((target, reaction, remove)) =
                    to_reaction(manager, &mut names, content).await
                {
                    apply_reaction(&mut messages, &target, reaction, remove);
                } else if let Some(message) =
                    to_chat_message(manager, &mut names, id.clone(), content).await
                {
                    messages.push(message);
                }
            }
            let _ = events.send(Event::History {
                conversation: id,
                messages,
            });
        }
        Command::React {
            conversation,
            target,
            emoji,
            remove,
        } => {
            let Some(thread) = threads.borrow().get(&conversation).cloned() else {
                return Err("unknown conversation".into());
            };
            let message = DataMessage {
                reaction: Some(presage::proto::data_message::Reaction {
                    emoji: Some(emoji.clone()),
                    remove: Some(remove),
                    target_author_aci: Some(target.author_id.clone()),
                    target_sent_timestamp: Some(target.timestamp),
                    ..Default::default()
                }),
                ..Default::default()
            };
            send_data(manager, &thread, message, now_millis()).await?;
            let reaction = Reaction {
                emoji,
                sender_id: own_id(manager).service_id_string(),
                sender: "You".into(),
                outgoing: true,
            };
            let _ = events.send(Event::Reaction {
                conversation,
                target,
                reaction,
                remove,
            });
        }
        Command::OpenAttachment(attachment) => {
            tokio::task::spawn_local(attachments::open(
                servers,
                attachment,
                events.clone(),
                downloads.clone(),
            ));
        }
        Command::Send {
            conversation,
            body,
            quote,
            files,
        } => {
            let Some(thread) = threads.borrow().get(&conversation).cloned() else {
                return Err("unknown conversation".into());
            };
            let pointers = attachments::upload(manager, &files).await?;
            // presage fills in the data message's timestamp from the one passed here.
            let timestamp = now_millis();
            let message = DataMessage {
                body: (!body.is_empty()).then(|| body.clone()),
                attachments: pointers.clone(),
                quote: quote
                    .as_ref()
                    .map(|quote| presage::proto::data_message::Quote {
                        id: Some(quote.timestamp),
                        author_aci: Some(quote.author_id.clone()),
                        text: Some(quote.text.clone()),
                        ..Default::default()
                    }),
                ..Default::default()
            };
            send_data(manager, &thread, message, timestamp).await?;
            let _ = events.send(Event::Message(ChatMessage {
                conversation,
                sender_id: manager
                    .registration_data()
                    .service_ids
                    .aci()
                    .service_id_string(),
                sender: "You".into(),
                body,
                timestamp,
                outgoing: true,
                quote,
                attachments: pointers
                    .iter()
                    .map(|pointer| attachments::describe(pointer, false))
                    .collect(),
                ..Default::default()
            }));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_refused_credentials_mean_the_device_was_removed() {
        use presage::libsignal_service::push_service::ServiceError;
        use reqwest_websocket::{Error as WsError, HandshakeError};
        let refused = |status: u16| {
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            PresageError::ServiceError(ServiceError::WsError(Box::new(WsError::Handshake(
                HandshakeError::UnexpectedStatusCode(status),
            ))))
        };
        assert!(removed_from_account(&refused(403)));
        assert!(removed_from_account(&refused(401)));
        assert!(!removed_from_account(&refused(500)));
        assert!(!removed_from_account(&refused(429)));
        assert!(removed_from_account(&PresageError::ServiceError(
            ServiceError::Unauthorized
        )));
        assert!(!removed_from_account(&PresageError::ServiceError(
            ServiceError::Timeout { reason: "slow" }
        )));
    }

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
        let dir = std::env::temp_dir().join(format!("langis-db-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
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
        assert!(open_store(&runtime, &local, &paths, &wrong).is_err());
        assert!(open_store(&runtime, &local, &paths, &key).is_ok());
        // Encrypted: no plaintext SQLite header.
        let bytes = std::fs::read(&paths.db).unwrap();
        assert!(!bytes.starts_with(b"SQLite format 3"));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
