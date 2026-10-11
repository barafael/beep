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
use presage::libsignal_service::prelude::ProtobufMessage as _;
use presage::{
    Error as PresageError, Manager,
    libsignal_service::zkgroup::groups::{GroupMasterKey, GroupSecretParams},
    libsignal_service::{
        configuration::SignalServers,
        content::{Content, ContentBody, DataMessage, GroupContextV2},
        protocol::ServiceId,
        push_service::ServiceError,
    },
    manager::Registered,
    model::{identity::OnNewIdentity, messages::Received},
    proto::{
        BodyRange, EditMessage, ReceiptMessage, SyncMessage, TypingMessage, body_range,
        data_message::{Delete, Flags},
        receipt_message,
        sync_message::{self, Content as SyncContent, Sent},
        typing_message,
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
    ChatMessage, ChatSettings, Command, Config, Conversation, ConversationId, Delivery, Event,
    Events, HistoryTransfer, Inbox, MessageRef, Password, Picture, Preferences, QrMatrix, Quote,
    Reaction, Style, StyleRange, apply_reaction, attachments,
    format::{self, RangeValue, WireRange},
    local::LocalStore,
    now_millis, transfer,
    vault::{self, KeySource, KeyringKey},
};

pub(crate) type Mgr = Manager<SqliteStore, Registered>;

/// Maps the opaque ids handed to the UI back to presage threads.
type Threads = Rc<RefCell<HashMap<ConversationId, Thread>>>;

pub fn run(config: Config, commands: Inbox, events: Events) {
    attachments::clean_opened();
    // Kept across unlocking, linking and relinking, which take commands too.
    let preferences = Rc::new(Cell::new(config.preferences));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    let local = tokio::task::LocalSet::new();
    let (result, closed) = match unlock(&runtime, &local, &commands, &events, &preferences) {
        Ok(Some((store, tables, closed))) => (
            local.block_on(
                &runtime,
                session(config, store, tables, commands, events.clone(), preferences),
            ),
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
    attachments::clean_opened();
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
        // `B33P_DATA_DIR` keeps a separate account, e.g. to link a second device for testing.
        // Its database key gets its own keyring entry, so it never replaces the default one.
        // A development aid: release builds always use the standard place.
        let custom = if cfg!(debug_assertions) {
            std::env::var_os("B33P_DATA_DIR")
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
                directories::ProjectDirs::from("io.github", "barafael", "b33p")
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
        match vault::restrict_file(&dir.join("b33p.key")) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                warn!(%error, "could not make the key source private");
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
}

/// Obtains the database key and opens the store; `None` if the UI quit while being asked for
/// a password. Keyring access blocks (D-Bus on Linux), so this runs outside the runtime and
/// only enters it to open the database.
fn unlock(
    runtime: &Runtime,
    local: &LocalSet,
    commands: &Inbox,
    events: &Events,
    preferences: &Cell<Preferences>,
) -> Result<Option<(SqliteStore, LocalStore, Closed)>, String> {
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
        let Some(password) = wait_for_password(commands, preferences) else {
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

fn wait_for_password(commands: &Inbox, preferences: &Cell<Preferences>) -> Option<Password> {
    // Nothing else can be done before the database is open, so other commands are dropped.
    commands.iter().find_map(|command| match command {
        Command::Unlock(password) => Some(password),
        Command::Preferences(changed) => {
            preferences.set(changed);
            None
        }
        _ => None,
    })
}

fn open_store(
    runtime: &Runtime,
    local: &LocalSet,
    paths: &Paths,
    key: &vault::Key,
) -> Result<(SqliteStore, LocalStore, Closed), String> {
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
        .collation("b33p_close_guard", move |a, b| {
            let _ = &open;
            a.cmp(b)
        });
    let store = local
        .block_on(
            runtime,
            SqliteStore::open_with_options(options.clone(), OnNewIdentity::Trust),
        )
        .map_err(|e| format!("failed to open the database: {e}"))?;
    // Same options, so the same key and close guard.
    let tables = local
        .block_on(runtime, LocalStore::open(options))
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
    Ok((store, tables, closed))
}

async fn session(
    config: Config,
    store: SqliteStore,
    tables: LocalStore,
    commands: Inbox,
    events: Events,
    preferences: Rc<Cell<Preferences>>,
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
            link_with_history(
                store.clone(),
                &config,
                &events,
                &mut command_rx,
                &paths,
                &preferences,
            )
            .await?
        }
        Err(PresageError::NotYetRegisteredError) => {
            link_with_history(
                store.clone(),
                &config,
                &events,
                &mut command_rx,
                &paths,
                &preferences,
            )
            .await?
        }
        Err(error) => return Err(format!("failed to load registration: {error}")),
    };
    // Runs until the UI goes away; if Signal removes this device meanwhile, links it again.
    loop {
        // `None`: the UI went away while linking.
        let Some((manager, pending)) = linked else {
            return Ok(());
        };
        if !run_linked(
            manager,
            pending,
            &events,
            &tables,
            &mut command_rx,
            &preferences,
        )
        .await
        {
            return Ok(());
        }
        let _ = events.send(Event::Error(
            "This device was removed from your Signal account, so it can't send or receive any more. \
             Scan the code to link it again; the messages here are kept."
                .into(),
        ));
        // Linking clears the dead registration; the conversations stay.
        linked = link_with_history(
            store.clone(),
            &config,
            &events,
            &mut command_rx,
            &paths,
            &preferences,
        )
        .await?;
    }
}

/// Runs a linked session: syncs, receives and handles commands. Returns `false` once the UI
/// has gone away, `true` if Signal removed this device and it has to be linked again.
async fn run_linked(
    mut manager: Mgr,
    pending: Vec<Command>,
    events: &Events,
    tables: &LocalStore,
    command_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Command>,
    preferences: &Rc<Cell<Preferences>>,
) -> bool {
    let ctx = Ctx {
        threads: Rc::default(),
        events: events.clone(),
        tables: tables.clone(),
        preferences: preferences.clone(),
        expiring: Rc::default(),
        storage_changed: Rc::default(),
        trust: Rc::default(),
    };
    let (threads, events) = (ctx.threads.clone(), ctx.events.clone());
    if send_conversations(&manager, &ctx).await == 0 {
        // Phones don't push their contacts to a newly linked device; they have to be asked.
        // The answer arrives through `receive` as `Received::Contacts`.
        info!("no contacts yet; requesting them from the primary device");
        if let Err(error) = manager.request_contacts().await {
            warn!(%error, "failed to request contacts");
        }
    }

    send_stories(&manager, &ctx).await;
    // In the background, so receiving starts right away.
    let latest = tokio::task::spawn_local(send_latest_messages(manager.clone(), ctx.clone()));
    let sweeping = tokio::task::spawn_local(sweep_expired(manager.clone(), ctx.clone()));
    let following = tokio::task::spawn_local(follow_storage(manager.clone(), ctx.clone()));
    let stories = tokio::task::spawn_local(expire_stories(manager.clone(), ctx.clone()));

    let downloads = attachments::download_limit();
    let avatars = AvatarFetcher::new(manager.clone(), threads.clone(), events.clone());
    avatars.request();
    let removed = Rc::new(tokio::sync::Notify::new());
    let receiving = tokio::task::spawn_local(receive(
        manager.clone(),
        ctx.clone(),
        avatars,
        removed.clone(),
    ));

    for command in pending {
        if let Err(error) = handle(&mut manager, &ctx, &downloads, command).await {
            warn!(%error, "command failed");
            let _ = events.send(Event::Error(error));
        }
    }
    let was_removed = loop {
        tokio::select! {
            command = command_rx.recv() => match command {
                Some(command) => {
                    if let Err(error) = handle(&mut manager, &ctx, &downloads, command).await {
                        warn!(%error, "command failed");
                        let _ = events.send(Event::Error(error));
                    }
                }
                None => break false,
            },
            () = removed.notified() => break true,
        }
    };
    latest.abort();
    sweeping.abort();
    following.abort();
    stories.abort();
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
    preferences: &Cell<Preferences>,
) -> Result<Option<(Mgr, Vec<Command>)>, String> {
    loop {
        // Nothing can be done before linking, so commands are dropped until then.
        let ui_closed = async {
            while let Some(command) = commands.recv().await {
                if let Command::Preferences(changed) = command {
                    preferences.set(changed);
                }
            }
        };
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
        Manager::link_secondary_device(
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
                        "The linking code couldn't be drawn. Restart b33p to try again.".into(),
                    ),
                });
            }
        },
    )
    .await;
    let manager = linked.map_err(|e| format!("linking failed: {e}"))?;
    let history_key = manager.registration_data().ephemeral_backup_key();
    Ok((manager, history_key))
}

fn conversation_id(thread: &Thread) -> ConversationId {
    ConversationId(match thread {
        Thread::Contact(id) => format!("c:{}", id.service_id_string()),
        Thread::Group(key) => format!("g:{}", hex::encode(key)),
    })
}

/// Remembers how to reach a conversation and describes it for the UI; settings are added by
/// [`with_settings`].
fn register(
    threads: &Threads,
    thread: Thread,
    title: String,
    subtitle: String,
    expire_timer: u32,
) -> Conversation {
    let id = conversation_id(&thread);
    let is_group = matches!(thread, Thread::Group(_));
    threads.borrow_mut().insert(id.clone(), thread);
    let title = crate::format::name(&title);
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
        settings: ChatSettings::default(),
        expire_timer,
        ..Conversation::default()
    }
}

/// Adds `thread` to the list of whom we block, the way current Signal apps read it. Those known
/// only by their phone `number` are listed by it; left out, the phone would unblock them, as it
/// takes the list as the whole one.
fn list_blocked(blocked: &mut sync_message::Blocked, thread: &Thread, number: Option<String>) {
    match thread {
        Thread::Contact(who @ ServiceId::Aci(_)) => {
            blocked.acis.push(who.service_id_string());
            blocked.acis_binary.push(who.service_id_binary());
        }
        Thread::Contact(_) => blocked.numbers.extend(number),
        Thread::Group(key) => blocked.group_ids.push(group_identifier(key).to_vec()),
    }
}

/// A contact's phone number as Signal sends it: "+4915123456789".
async fn e164_of(manager: &Mgr, who: ServiceId) -> Option<String> {
    use presage::libsignal_service::prelude::phonenumber::Mode;
    let contact = manager.store().contact_by_id(&who).await.ok()??;
    Some(contact.phone_number?.format().mode(Mode::E164).to_string())
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

/// What the parts of a linked session share.
#[derive(Clone)]
struct Ctx {
    threads: Threads,
    events: Events,
    tables: LocalStore,
    preferences: Rc<Cell<Preferences>>,
    /// Woken when a disappearing message starts counting down, for `sweep_expired`.
    expiring: Rc<tokio::sync::Notify>,
    /// Woken when the phone says the storage service changed, for `follow_storage`.
    storage_changed: Rc<tokio::sync::Notify>,
    /// How far each contact's safety number is trusted, as the UI was last told; see
    /// [`publish_trust`].
    trust: Rc<RefCell<HashMap<ServiceId, crate::Trust>>>,
}

/// The thread a conversation id stands for; see [`conversation_id`].
fn thread_of(id: &ConversationId) -> Option<Thread> {
    if let Some(service_id) = id.0.strip_prefix("c:") {
        return ServiceId::parse_from_service_id_string(service_id).map(Thread::Contact);
    }
    let key = hex::decode(id.0.strip_prefix("g:")?).ok()?;
    Some(Thread::Group(key.try_into().ok()?))
}

/// The id typing indicators and the like name a group by.
fn group_identifier(master_key: &[u8; 32]) -> [u8; 32] {
    GroupSecretParams::derive_from_master_key(GroupMasterKey::new(*master_key))
        .get_group_identifier()
}

/// Adds what b33p remembers about each conversation.
async fn with_settings(ctx: &Ctx, conversations: &mut [Conversation]) {
    let mut remembered = ctx.tables.conversations().await.unwrap_or_else(|error| {
        warn!(%error, "failed to read conversation settings");
        HashMap::new()
    });
    for conversation in conversations {
        conversation.settings = remembered.remove(&conversation.id).unwrap_or_default();
    }
}

/// A thread's disappearing-message timer (seconds; 0 for off).
async fn expire_timer(manager: &Mgr, thread: &Thread) -> u32 {
    match manager.store().expire_timer(thread).await {
        Ok(Some((timer, _))) => timer,
        _ => 0,
    }
}

async fn send_conversations(manager: &Mgr, ctx: &Ctx) -> usize {
    let threads = &ctx.threads;
    let store = manager.store();
    let mut all = Vec::new();
    // Group members are mostly contacts; others are looked up once each.
    let mut names = Names::new();
    // Only verified contacts can have a changed safety number; the rest need no key lookup.
    let verifications = ctx.tables.verifications().await.unwrap_or_default();
    // The store's pins follow what is kept (also for verifications from before pinning).
    for (aci, verification) in &verifications {
        if let Some(who) = ServiceId::parse_from_service_id_string(aci) {
            pin_verified(manager, who, verification).await;
        }
    }
    if let Ok(contacts) = store.contacts().await {
        for contact in contacts.flatten() {
            let thread = Thread::Contact(ServiceId::Aci(contact.uuid.into()));
            let number = contact
                .phone_number
                .as_ref()
                .map(display_number)
                .unwrap_or_default();
            let name = crate::format::name(&contact.name);
            let title = if name.is_empty() {
                number.clone()
            } else {
                name
            };
            let timer = contact.expire_timer;
            names.insert(
                ServiceId::Aci(contact.uuid.into()).service_id_string(),
                title.clone(),
            );
            let who = ServiceId::Aci(contact.uuid.into());
            let mut conversation = register(threads, thread, title, number, timer);
            conversation.trust =
                trust_given(manager, who, verifications.get(&who.service_id_string())).await;
            ctx.trust.borrow_mut().insert(who, conversation.trust);
            all.push(conversation);
        }
    }
    let contacts = all.len();
    // Messages to yourself; listed once there are some.
    let own = Thread::Contact(own_id(manager));
    let timer = expire_timer(manager, &own).await;
    let mut note_to_self = register(
        threads,
        own,
        "Note to Self".into(),
        "Messages to yourself".into(),
        timer,
    );
    note_to_self.note_to_self = true;
    all.push(note_to_self);
    if let Ok(groups) = store.groups().await {
        for (key, group) in groups.flatten() {
            all.push(group_conversation(manager, threads, &mut names, key, group).await);
        }
    }
    with_settings(ctx, &mut all).await;
    let _ = ctx.events.send(Event::Conversations(all));
    if let Ok(folders) = ctx.tables.folders().await {
        let _ = ctx.events.send(Event::Folders(folders));
    }
    contacts
}

/// A group as a conversation, registered: title, members, our role, timer.
async fn group_conversation(
    manager: &Mgr,
    threads: &Threads,
    names: &mut Names,
    key: [u8; 32],
    group: presage::model::groups::Group,
) -> Conversation {
    use presage::libsignal_service::groups_v2::Role;
    let own = manager.registration_data().service_ids.aci();
    let us = group.members.iter().find(|m| m.aci == own);
    let left = us.is_none();
    let admin = us.is_some_and(|m| m.role == Role::Administrator);
    let timer = group
        .disappearing_messages_timer
        .as_ref()
        .map_or(0, |t| t.duration);
    let people = people(manager, names, &group).await;
    // Invited (by our ACI, or by our PNI if someone had only our number).
    let ids = &manager.registration_data().service_ids;
    let invited = left
        && group.pending_members.iter().any(|m| {
            use presage::model::ServiceIdType;
            match m.service_id_type {
                ServiceIdType::AccountIdentity => m.uuid == ids.aci,
                ServiceIdType::PhoneNumberIdentity => m.uuid == ids.pni,
            }
        });
    let subtitle = if invited {
        "You're invited to this group.".into()
    } else if left {
        "You aren't a member of this group.".into()
    } else {
        members(group.members.len())
    };
    let mut conversation = register(threads, Thread::Group(key), group.title, subtitle, timer);
    conversation.members = people;
    conversation.admin = admin;
    conversation.left = left;
    conversation.invited = invited;
    conversation
}

/// Tells the UI how a group is now, e.g. after a change.
async fn refresh_group(manager: &Mgr, ctx: &Ctx, key: [u8; 32]) {
    let Ok(Some(group)) = manager.store().group(key).await else {
        return;
    };
    let mut conversation =
        group_conversation(manager, &ctx.threads, &mut Names::new(), key, group).await;
    let id = conversation.id.clone();
    conversation.settings = ctx
        .tables
        .settings(&id)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let _ = ctx.events.send(Event::Conversations(vec![conversation]));
}

/// The group change a message carries, decrypted with the group's key, if the server signed
/// it. Only a signed change counts: the bytes are the sender's, so any member could make up
/// one that was never applied (removals, admin rights, a timer) and have it narrated or acted
/// on. Changes made here go out with the server's signature too (presage sends the change
/// the server returned), so they pass.
async fn signed_group_change(
    manager: &Mgr,
    key: [u8; 32],
    context: &GroupContextV2,
) -> Option<presage::libsignal_service::groups_v2::GroupChanges> {
    use presage::libsignal_service::groups_v2::GroupOperations;
    let change = presage::proto::GroupChange::decode(context.group_change.as_deref()?).ok()?;
    let servers = presage::libsignal_service::configuration::ServiceConfiguration::from(
        manager.registration_data().signal_servers,
    );
    let signed = <[u8; 64]>::try_from(change.server_signature.as_slice()).is_ok_and(|signature| {
        servers
            .zkgroup_server_public_params
            .verify_signature(&change.actions, signature)
            .is_ok()
    });
    if !signed {
        debug!("a group change without the server's signature; ignored");
        return None;
    }
    let operations = GroupOperations::new(GroupSecretParams::derive_from_master_key(
        GroupMasterKey::new(key),
    ));
    operations.decrypt_group_change(change).ok()
}

/// If `context`'s change removed us, drops us from the stored group: presage can't read the
/// group from the server any more, so its copy would still count us in.
async fn forget_membership(manager: &Mgr, key: [u8; 32], context: &GroupContextV2) {
    use presage::libsignal_service::groups_v2::GroupChange;
    let own = manager.registration_data().service_ids.aci();
    let Some(changes) = signed_group_change(manager, key, context).await else {
        return;
    };
    let removed = changes
        .changes
        .iter()
        .any(|change| matches!(change, GroupChange::DeleteMember(aci) if *aci == own));
    let store = manager.store().clone();
    let Ok(Some(mut group)) = store.group(key).await else {
        return;
    };
    if removed && group.revision < changes.version {
        group.members.retain(|member| member.aci != own);
        group.revision = changes.version;
        if let Err(error) = store.save_group(key, group).await {
            warn!(%error, "failed to note that we were removed from a group");
        }
    }
}

/// Stories last a day.
const STORY_LIFETIME: u64 = 24 * 60 * 60 * 1000;
/// The colours of text stories posted here: white on Signal's blue.
const STORY_FOREGROUND: u32 = 0xffff_ffff;
const STORY_BACKGROUND: u32 = 0xff3a_76f0;

/// Keeps a story from someone (or from us, on another device) and tells the UI.
async fn on_story(
    manager: &Mgr,
    ctx: &Ctx,
    author: ServiceId,
    timestamp: u64,
    story: &presage::proto::StoryMessage,
    outgoing: bool,
) {
    use presage::proto::{story_message::Attachment, text_attachment::Background};
    if now_millis().saturating_sub(timestamp) > STORY_LIFETIME {
        return;
    }
    // A group story, or one to the author's contacts.
    let conversation = match story
        .group
        .as_ref()
        .and_then(|g| g.master_key.clone())
        .and_then(|key| <[u8; 32]>::try_from(key).ok())
    {
        Some(key) => conversation_id(&Thread::Group(key)),
        None => ConversationId::contact(&author.service_id_string()),
    };
    let mut stored = crate::local::StoredStory {
        author: author.service_id_string(),
        timestamp,
        conversation,
        viewed: outgoing,
        outgoing,
        ..Default::default()
    };
    match &story.attachment {
        Some(Attachment::FileAttachment(pointer)) => {
            stored.pointer = Some(pointer.encode_to_vec());
        }
        Some(Attachment::TextAttachment(text)) => {
            stored.text = text.text.clone().unwrap_or_default();
            stored.foreground = text.text_foreground_color.unwrap_or(STORY_FOREGROUND);
            stored.background = match &text.background {
                Some(Background::Color(color)) => *color,
                // A gradient's first colour.
                Some(Background::Gradient(gradient)) => gradient
                    .colors
                    .first()
                    .copied()
                    .or(gradient.start_color)
                    .unwrap_or(STORY_BACKGROUND),
                None => STORY_BACKGROUND,
            };
        }
        None => return,
    }
    if let Err(error) = ctx.tables.add_story(&stored).await {
        warn!(%error, "failed to keep a story");
        return;
    }
    send_stories(manager, ctx).await;
}

/// Tells the UI the stories of the last day.
async fn send_stories(manager: &Mgr, ctx: &Ctx) {
    let since = now_millis().saturating_sub(STORY_LIFETIME);
    let Ok(stored) = ctx.tables.stories(since).await else {
        return;
    };
    let mut names = Names::new();
    let mut stories = Vec::new();
    for story in stored {
        // Not from those we block, also in groups. (Strangers' are left out when listing.)
        let by_author = ConversationId::contact(&story.author);
        if !story.outgoing
            && (is_blocked(ctx, &by_author).await || is_blocked(ctx, &story.conversation).await)
        {
            continue;
        }
        let author = match ServiceId::parse_from_service_id_string(&story.author) {
            Some(who) => display_name(manager, &mut names, who).await,
            None => String::new(),
        };
        let content = match &story.pointer {
            Some(pointer) => match presage::proto::AttachmentPointer::decode(&pointer[..]) {
                Ok(pointer) => crate::StoryContent::Picture(attachments::describe(&pointer, false)),
                Err(_) => continue,
            },
            None => crate::StoryContent::Text {
                text: story.text,
                foreground: story.foreground,
                background: story.background,
            },
        };
        stories.push(crate::Story {
            author_id: story.author,
            author,
            conversation: story.conversation,
            timestamp: story.timestamp,
            content,
            viewed: story.viewed,
            outgoing: story.outgoing,
        });
    }
    let _ = ctx.events.send(Event::Stories(stories));
}

/// Tells a story's author we viewed it (with `receipts`), and our other devices.
async fn send_viewed(mut manager: Mgr, story: MessageRef, receipts: bool) {
    let timestamp = now_millis();
    let Some(author) = ServiceId::parse_from_service_id_string(&story.author_id) else {
        return;
    };
    if author == own_id(&manager) {
        return;
    }
    if receipts {
        let receipt = ReceiptMessage {
            r#type: Some(receipt_message::Type::Viewed as i32),
            timestamp: vec![story.timestamp],
        };
        if let Err(error) = manager
            .send_message(author, ContentBody::ReceiptMessage(receipt), timestamp)
            .await
        {
            debug!(%error, "failed to send a viewed receipt");
        }
    }
    let sync = SyncMessage {
        viewed: vec![sync_message::Viewed {
            sender_aci_binary: aci_binary(&story.author_id),
            sender_aci: Some(story.author_id),
            timestamp: Some(story.timestamp),
        }],
        ..Default::default()
    };
    send_sync(&mut manager, sync).await;
}

/// Why someone can't be put in a group.
const PHONE_NUMBER_ONLY: &str = "someone chosen is known only by their phone number: they can be \
     invited once the group exists (make it, then add them)";

/// A group member's ACI; `None` for those known only by their phone number (PNI).
fn member_aci(id: &str) -> Option<presage::libsignal_service::protocol::Aci> {
    match ServiceId::parse_from_service_id_string(id)? {
        ServiceId::Aci(aci) => Some(aci),
        ServiceId::Pni(_) => None,
    }
}

/// The message a reaction is to. Current Signal apps name its author only in binary.
fn reaction_target(reaction: &presage::proto::data_message::Reaction) -> Option<MessageRef> {
    Some(MessageRef {
        author_id: ServiceId::Aci(reaction.parse_target_author_aci()?).service_id_string(),
        timestamp: reaction.target_sent_timestamp?,
    })
}

/// An ACI in the binary form current Signal apps read: they ignore the string fields b33p also
/// fills, for older clients. `None` for anything but an ACI.
fn aci_binary(id: &str) -> Option<Vec<u8>> {
    member_aci(id).map(|aci| ServiceId::Aci(aci).service_id_binary())
}

/// Refuses to leave a group we are the only admin of, while others are in it: it would be
/// left without one. Signal's apps ask to choose a new admin first; here it is done by hand.
async fn only_admin_check(manager: &Mgr, key: [u8; 32]) -> Result<(), String> {
    use presage::libsignal_service::groups_v2::Role;
    let own = manager.registration_data().service_ids.aci();
    let Ok(Some(group)) = manager.store().group(key).await else {
        return Ok(());
    };
    let admin = |m: &&presage::model::groups::Member| m.role == Role::Administrator;
    let we_admin = group.members.iter().filter(admin).any(|m| m.aci == own);
    let other_admin = group.members.iter().filter(admin).any(|m| m.aci != own);
    let others = group.members.iter().any(|m| m.aci != own);
    if we_admin && !other_admin && others {
        return Err("you are the group's only admin: make someone else an admin first".into());
    }
    Ok(())
}

/// A group change for presage; `None` if a member isn't an ACI.
fn group_edit(edit: crate::GroupEdit) -> Option<presage::manager::GroupEdit> {
    use crate::GroupEdit as Edit;
    use presage::manager::GroupEdit as To;
    let aci = member_aci;
    Some(match edit {
        Edit::Title(title) => To::Title(title.trim().to_owned()),
        Edit::Description(description) => To::Description(description.trim().to_owned()),
        // Someone known only by phone number is invited by it.
        Edit::AddMembers(members) => To::AddMembers(
            members
                .iter()
                .map(|m| ServiceId::parse_from_service_id_string(m))
                .collect::<Option<_>>()?,
        ),
        Edit::RemoveMember(member) => To::RemoveMember(aci(&member)?),
        Edit::SetAdmin(member, admin) => To::SetAdmin(aci(&member)?, admin),
        Edit::Leave => To::Leave,
        Edit::AcceptInvitation => To::AcceptInvitation,
        Edit::DeclineInvitation => To::DeclineInvitation,
    })
}

/// Shows a message we stored ourselves (e.g. a group change), as it will read after a restart.
async fn show_stored(
    manager: &Mgr,
    ctx: &Ctx,
    conversation: &ConversationId,
    thread: &Thread,
    timestamp: u64,
) {
    if let Ok(Some(content)) = manager.store().message(thread, timestamp).await
        && let Some(message) =
            to_chat_message(manager, &mut Names::new(), conversation.clone(), &content).await
    {
        let _ = ctx.events.send(Event::Message(message));
    }
}

/// What a group change says, from the change itself (decrypted with the group's key, and only
/// if the server signed it; see [`signed_group_change`]), and the timer it sets, if it does.
/// `editor` is who made it.
async fn group_notice(
    manager: &Mgr,
    names: &mut Names,
    context: &GroupContextV2,
    editor: &str,
) -> Option<(String, Option<u32>)> {
    use presage::libsignal_service::groups_v2::{GroupChange, Role};
    let key: [u8; 32] = context.master_key.clone()?.try_into().ok()?;
    if context.group_change.is_none() {
        // A group's first message, from whoever made it.
        return (context.revision == Some(0)).then(|| {
            let text = if editor == "You" {
                "You created the group.".to_string()
            } else {
                format!("{editor} added you to the group.")
            };
            (text, None)
        });
    }
    let changes = signed_group_change(manager, key, context).await?;
    let editor_id = ServiceId::Aci(changes.editor);
    let editor = display_name(manager, names, editor_id).await;
    let mut lines = Vec::new();
    let mut timer = None;
    for change in changes.changes {
        let line = match change {
            GroupChange::NewMember(member) => {
                let name = display_name(manager, names, ServiceId::Aci(member.aci)).await;
                if ServiceId::Aci(member.aci) == editor_id {
                    format!("{editor} joined the group.")
                } else {
                    format!("{editor} added {name}.")
                }
            }
            GroupChange::DeleteMember(aci) => {
                let name = display_name(manager, names, ServiceId::Aci(aci)).await;
                if ServiceId::Aci(aci) == editor_id {
                    format!("{name} left the group.")
                } else {
                    format!("{editor} removed {name}.")
                }
            }
            GroupChange::ModifyMemberRole { aci, role } => {
                let name = display_name(manager, names, ServiceId::Aci(aci)).await;
                if role == Role::Administrator {
                    format!("{editor} made {name} an admin.")
                } else {
                    format!("{editor} took {name}'s admin rights.")
                }
            }
            GroupChange::NewPendingMember(_) => format!("{editor} invited someone."),
            GroupChange::PromotePendingMember { address, .. } => {
                let name = display_name(manager, names, address).await;
                format!("{name} joined the group.")
            }
            // Invited by phone number, and joined.
            GroupChange::PromotePendingPniAciMemberProfileKey(member) => {
                let name = display_name(manager, names, ServiceId::Aci(member.aci)).await;
                format!("{name} joined the group.")
            }
            GroupChange::DeletePendingMember(invitee) => {
                // Our own invitation may be for our PNI, which only we can tell is us.
                let own_pni = ServiceId::Pni(manager.registration_data().service_ids.pni());
                if invitee == editor_id || (editor_id == own_id(manager) && invitee == own_pni) {
                    format!("{editor} declined the invitation.")
                } else {
                    format!("{editor} withdrew an invitation.")
                }
            }
            GroupChange::Title(title) => format!("{editor} named the group “{title}”."),
            GroupChange::Description(_) => format!("{editor} changed the group's description."),
            GroupChange::Avatar(_) => format!("{editor} changed the group's picture."),
            GroupChange::Timer(t) => {
                let seconds = t.map_or(0, |t| t.duration);
                timer = Some(seconds);
                crate::timer_notice(&editor, seconds)
            }
            _ => continue,
        };
        lines.push(line);
    }
    (!lines.is_empty()).then(|| (lines.join("\n"), timer))
}

/// A group's members other than us, by name.
async fn people(
    manager: &Mgr,
    names: &mut Names,
    group: &presage::model::groups::Group,
) -> Vec<crate::Person> {
    let own = own_id(manager);
    let mut people = Vec::new();
    for member in &group.members {
        let who = ServiceId::Aci(member.aci);
        if who != own {
            people.push(crate::Person {
                service_id: who.service_id_string(),
                name: sender_name(manager, names, who).await,
            });
        }
    }
    people.sort_by_cached_key(|p| p.name.to_lowercase());
    people
}

/// Sends each conversation's newest stored message, so the UI can order conversations by
/// activity and show previews before any history is opened; then how many messages in each
/// are unread.
async fn send_latest_messages(manager: Mgr, ctx: Ctx) {
    let threads: Vec<_> = ctx
        .threads
        .borrow()
        .iter()
        .map(|(id, thread)| (id.clone(), thread.clone()))
        .collect();
    let mut names = Names::new();
    let mut unread = Vec::new();
    let mut remembered = match ctx.tables.conversations().await {
        Ok(remembered) => remembered,
        Err(error) => {
            warn!(%error, "failed to read conversation settings");
            return;
        }
    };
    for (id, thread) in threads {
        // Only for the preview and the order: `LoadHistory` replaces it with the full message
        // (delivery, edits, …).
        let Some(latest) = latest_message(&manager, &mut names, &id, &thread).await else {
            continue;
        };
        let timestamp = latest.timestamp;
        if !ctx.events.send(Event::Latest(latest)) {
            return;
        }
        match remembered.remove(&id) {
            Some(settings) => {
                let count = count_unread(&manager, &thread, settings.last_read).await;
                if count > 0 {
                    unread.push((id, count));
                }
            }
            // New to b33p (first start, or just linked with history): what is there counts
            // as read, or every chat would start out unread.
            None => {
                if let Err(error) = ctx.tables.mark_read(&id, timestamp).await {
                    warn!(%error, "failed to remember a conversation");
                }
            }
        }
    }
    let _ = ctx.events.send(Event::Unread(unread));
}

/// A conversation's newest stored message.
///
/// The store can only return whole time ranges, so this looks at the last week first and only
/// reaches further back for quiet conversations, instead of reading every chat's full history.
async fn latest_message(
    manager: &Mgr,
    names: &mut Names,
    id: &ConversationId,
    thread: &Thread,
) -> Option<ChatMessage> {
    // Receipts and the like are stored too; look past them a few at a time.
    let mut before = None;
    loop {
        let contents = manager
            .store()
            .messages_before(thread, before, 20)
            .await
            .ok()?;
        for content in &contents {
            if let Some(message) = to_chat_message(manager, names, id.clone(), content).await {
                return Some(message);
            }
        }
        before = Some(stored_at(contents.last()?));
    }
}

/// When a content was stored: what the store orders and pages by.
fn stored_at(content: &Content) -> u64 {
    content.metadata.client_timestamp.timestamp_millis() as u64
}

/// Messages shown per page of history.
const HISTORY_PAGE: usize = 100;

/// Stored contents of a thread before `before` (or the newest), newest first: enough for
/// [`HISTORY_PAGE`] messages, plus where the next older page starts, if there is one.
async fn history_page(
    manager: &Mgr,
    thread: &Thread,
    before: Option<u64>,
) -> Result<(Vec<Content>, Option<u64>), String> {
    // Receipts, reactions and the like are stored between messages; read a bit more.
    const BATCH: u32 = 200;
    let mut contents = Vec::new();
    let mut cursor = before;
    loop {
        let batch = manager
            .store()
            .messages_before(thread, cursor, BATCH)
            .await
            .map_err(|e| format!("failed to load history: {e}"))?;
        let exhausted = batch.len() < BATCH as usize;
        cursor = batch.last().map(stored_at).or(cursor);
        contents.extend(batch);
        if exhausted {
            return Ok((contents, None));
        }
        let messages = contents
            .iter()
            .filter(|c| data_of(c).is_some_and(|(data, _)| data.reaction.is_none()))
            .count();
        if messages >= HISTORY_PAGE {
            return Ok((contents, cursor));
        }
    }
}

/// Incoming messages in a thread after `last_read`.
async fn count_unread(manager: &Mgr, thread: &Thread, last_read: u64) -> u32 {
    let own = own_id(manager);
    let Ok(contents) = manager.store().messages(thread, last_read + 1..).await else {
        return 0;
    };
    let count = contents
        .flatten()
        .filter(|content| {
            content.metadata.sender != own
                && matches!(&content.body, ContentBody::DataMessage(data)
                    if data.reaction.is_none()
                        && (data.body.is_some() || !data.attachments.is_empty()))
        })
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// Adds what b33p keeps beside the store to messages of one conversation: how far ours got,
/// earlier versions of edited ones, when disappearing ones go.
async fn decorate(ctx: &Ctx, id: &ConversationId, messages: &mut [ChatMessage]) {
    let (deliveries, edits, expiries) = futures::join!(
        ctx.tables.deliveries(id),
        ctx.tables.edits(id),
        ctx.tables.expiries(id),
    );
    let (mut deliveries, mut edits, mut expiries) = (
        deliveries.unwrap_or_default(),
        edits.unwrap_or_default(),
        expiries.unwrap_or_default(),
    );
    for message in messages {
        if message.outgoing {
            message.delivery = deliveries.remove(&message.timestamp).unwrap_or_default();
        }
        let reference = message.reference();
        if let Some(earlier) = edits.remove(&reference) {
            message.edited_at = message.edited_at.max(earlier.last().map(|e| e.0));
            message.edits = earlier;
        }
        message.expires_at = expiries.remove(&reference);
    }
}

/// Takes over chat settings from the phone's storage service now, and each time the phone
/// says they changed, for the rest of the session.
async fn follow_storage(mut manager: Mgr, ctx: Ctx) {
    const RETRY: Duration = Duration::from_secs(5 * 60);
    // Changes made here come in bursts (pinning, reordering); write them together.
    const SETTLE: Duration = Duration::from_secs(2);
    loop {
        match crate::storage::sync(&manager, &ctx.tables, &ctx.events).await {
            Ok((known, verifications)) => {
                take_verifications(&manager, &ctx, verifications).await;
                if known && !ctx.events.send(Event::StorageRead) {
                    return;
                }
                if known && ctx.preferences.get().sync_to_phone {
                    write_storage(&mut manager, &ctx).await;
                }
                if known {
                    ctx.storage_changed.notified().await;
                } else {
                    // The phone sends the key right after linking (`handle_content` wakes
                    // this when it comes); if that was missed, look again later rather than
                    // never, since without it message requests can't be told apart.
                    tokio::select! {
                        () = tokio::time::sleep(RETRY) => {}
                        () = ctx.storage_changed.notified() => {}
                    }
                }
                tokio::time::sleep(SETTLE).await;
            }
            // E.g. offline at the start; try again later.
            Err(error) => {
                warn!(%error, "failed to read the storage service");
                tokio::select! {
                    () = tokio::time::sleep(RETRY) => {}
                    () = ctx.storage_changed.notified() => {}
                }
            }
        }
    }
}

/// Takes over the verifications the storage service says the phone changed (quietly on the
/// first read, which brings what was set before), then keeps what it said.
async fn take_verifications(
    manager: &Mgr,
    ctx: &Ctx,
    verifications: Option<crate::storage::Verifications>,
) {
    let Some(verifications) = verifications else {
        return;
    };
    for (who, verification) in &verifications.changed {
        let Ok(key) =
            presage::libsignal_service::protocol::IdentityKey::decode(&verification.identity_key)
        else {
            continue;
        };
        let verifier = Verifier::Elsewhere {
            quiet: verifications.first,
        };
        // One failing doesn't hold back the others: the storage service won't be read for them
        // again until the phone changes something.
        if let Err(error) =
            remember_verification(manager, ctx, *who, &key, verification.state, verifier).await
        {
            warn!(%error, "failed to keep a verification from the phone");
        }
    }
    if let Err(error) = ctx
        .tables
        .set_remote_verifications(&verifications.now)
        .await
    {
        warn!(%error, "failed to keep what the phone says about verifications");
    }
}

/// Writes what changed here to the storage service, reading first when the phone changed
/// something meanwhile; then tells our other devices to fetch it.
async fn write_storage(manager: &mut Mgr, ctx: &Ctx) {
    use crate::storage::Written;
    for _ in 0..3 {
        match crate::storage::push(manager, &ctx.tables).await {
            Ok(Written::Stale) => {
                match crate::storage::sync(manager, &ctx.tables, &ctx.events).await {
                    Ok((_, verifications)) => take_verifications(manager, ctx, verifications).await,
                    Err(error) => {
                        warn!(%error, "failed to read the storage service");
                        return;
                    }
                }
            }
            Ok(Written::Records(_)) => {
                use sync_message::fetch_latest::Type;
                let latest = sync_message::FetchLatest {
                    r#type: Some(Type::StorageManifest as i32),
                };
                let sync = SyncMessage {
                    content: Some(SyncContent::FetchLatest(latest)),
                    ..Default::default()
                };
                send_sync(manager, sync).await;
                return;
            }
            Ok(Written::Nothing | Written::NoKey) => return,
            Err(error) => {
                warn!(%error, "failed to write to the storage service");
                return;
            }
        }
    }
    warn!("the storage service kept changing; trying again later");
}

/// Takes stories off the list once their day is over, for the rest of the session.
async fn expire_stories(manager: Mgr, ctx: Ctx) {
    loop {
        tokio::time::sleep(Duration::from_secs(5 * 60)).await;
        send_stories(&manager, &ctx).await;
    }
}

/// Deletes disappearing messages when their time is up, for the rest of the session.
async fn sweep_expired(manager: Mgr, ctx: Ctx) {
    loop {
        let (due, next) = match ctx.tables.due(now_millis()).await {
            Ok(found) => found,
            Err(error) => {
                warn!(%error, "failed to look for disappearing messages");
                return;
            }
        };
        let mut deleted = Vec::new();
        let mut store = manager.store().clone();
        for (conversation, message) in due {
            if let Some(thread) = thread_of(&conversation)
                && let Err(error) = store.delete_message(&thread, message.timestamp).await
            {
                warn!(%error, "failed to delete a disappearing message");
                continue;
            }
            deleted.push((conversation, message));
        }
        if let Err(error) = ctx.tables.forget_expired(&deleted).await {
            warn!(%error, "failed to forget disappearing messages");
            return;
        }
        let mut gone: HashMap<ConversationId, Vec<MessageRef>> = HashMap::new();
        for (conversation, message) in deleted {
            gone.entry(conversation).or_default().push(message);
        }
        for (conversation, targets) in gone {
            if !ctx.events.send(Event::Expired {
                conversation,
                targets,
            }) {
                return;
            }
        }
        let wait = next.map_or(Duration::from_secs(24 * 3600), |at| {
            Duration::from_millis(at.saturating_sub(now_millis()))
        });
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = ctx.expiring.notified() => {}
        }
    }
}

/// Starts the timers (seconds) of disappearing messages in a conversation, as of `start`:
/// incoming ones once read, ours once sent.
async fn start_timers(
    ctx: &Ctx,
    conversation: &ConversationId,
    timers: &[(MessageRef, u32)],
    start: u64,
) {
    let expiries: Vec<_> = timers
        .iter()
        .filter(|(_, timer)| *timer > 0)
        .map(|(message, timer)| (message.clone(), start + u64::from(*timer) * 1000))
        .collect();
    if expiries.is_empty() {
        return;
    }
    if let Err(error) = ctx.tables.set_expiries(conversation, &expiries).await {
        warn!(%error, "failed to start disappearing timers");
        return;
    }
    ctx.expiring.notify_one();
    let _ = ctx.events.send(Event::Expiring {
        conversation: conversation.clone(),
        expiries,
    });
}

/// The disappearing timers of stored messages, for messages read elsewhere.
async fn stored_timers(
    manager: &Mgr,
    conversation: &ConversationId,
    messages: &[MessageRef],
) -> Vec<(MessageRef, u32)> {
    let Some(thread) = thread_of(conversation) else {
        return Vec::new();
    };
    let mut timers = Vec::new();
    for message in messages {
        if let Ok(Some(content)) = manager.store().message(&thread, message.timestamp).await
            && let Some(timer) = data_of(&content).and_then(|(data, _)| data.expire_timer)
        {
            timers.push((message.clone(), timer));
        }
    }
    timers
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

/// What group messages carry to say which group, and in which revision.
async fn group_context(manager: &Mgr, key: &[u8; 32]) -> GroupContextV2 {
    // Recipients compare the revision with their group state to spot stale context.
    let revision = match manager.store().group(*key).await {
        Ok(Some(group)) => group.revision,
        _ => 0,
    };
    GroupContextV2 {
        master_key: Some(key.to_vec()),
        revision: Some(revision),
        ..Default::default()
    }
}

/// Why sending failed: the store refused the identity a contact has now for sending (one other
/// than the verified one pinned for them, see [`pin_verified`]), or anything else.
enum SendError {
    Untrusted(ServiceId),
    Other(String),
}

impl From<SendError> for String {
    fn from(error: SendError) -> Self {
        match error {
            SendError::Untrusted(_) => {
                "Their safety number changed since you verified it: confirm it to send.".into()
            }
            SendError::Other(error) => error,
        }
    }
}

/// The contact whose identity a failed send refused, if that is why it failed: libsignal says
/// so directly, or libsignal-service when setting up a session with their new key.
fn untrusted(error: &PresageError<presage_store_sqlite::SqliteStoreError>) -> Option<ServiceId> {
    use presage::libsignal_service::{prelude::MessageSenderError, protocol::SignalProtocolError};
    let protocol = |error: &SignalProtocolError| match error {
        SignalProtocolError::UntrustedIdentity(address) => {
            ServiceId::parse_from_service_id_string(address.name())
        }
        _ => None,
    };
    match error {
        PresageError::MessageSenderError(error) => match error.as_ref() {
            MessageSenderError::UntrustedIdentity { address } => Some(*address),
            MessageSenderError::ProtocolError(error) => protocol(error),
            _ => None,
        },
        PresageError::ProtocolError(error) => protocol(error),
        _ => None,
    }
}

/// Sends to a contact, or to everyone in a group.
async fn send_to(
    manager: &mut Mgr,
    thread: &Thread,
    body: ContentBody,
    timestamp: u64,
) -> Result<(), SendError> {
    let sent = match thread {
        Thread::Contact(recipient) => manager.send_message(*recipient, body, timestamp).await,
        Thread::Group(key) => manager.send_message_to_group(key, body, timestamp).await,
    };
    sent.map_err(|error| match untrusted(&error) {
        Some(who) => SendError::Untrusted(who),
        None => SendError::Other(format!("failed to send: {error}")),
    })
}

/// [`send_to`], holding the chat of a contact whose identity it refused: their verified safety
/// number changed, which they may not have heard of yet (the new key came with sending).
/// Nothing goes to a group while that holds for one of its members: presage would reach the
/// others before refusing them, and report the message as failed, so a retry would repeat it.
async fn deliver(
    manager: &mut Mgr,
    ctx: &Ctx,
    thread: &Thread,
    body: ContentBody,
    timestamp: u64,
) -> Result<(), String> {
    if let Thread::Group(key) = thread {
        let held = held_members(manager, ctx, key).await;
        if !held.is_empty() {
            return Err(format!(
                "Not sent: your safety number with {} changed since you verified it. Confirm it \
                 in their chat first.",
                held.join(", ")
            ));
        }
    }
    match send_to(manager, thread, body, timestamp).await {
        Err(SendError::Untrusted(who)) => {
            hold_changed(manager, ctx, who).await;
            Err(SendError::Untrusted(who).into())
        }
        sent => sent.map_err(String::from),
    }
}

/// The names of the members of group `key` whose verified safety number changed and isn't
/// confirmed yet (see [`hold_changed`]), as the UI was last told.
async fn held_members(manager: &Mgr, ctx: &Ctx, key: &[u8; 32]) -> Vec<String> {
    let Ok(Some(group)) = manager.store().group(*key).await else {
        return Vec::new();
    };
    let held: Vec<ServiceId> = {
        let trust = ctx.trust.borrow();
        group
            .members
            .iter()
            .map(|member| ServiceId::Aci(member.aci))
            .filter(|who| trust.get(who) == Some(&crate::Trust::Changed))
            .collect()
    };
    let mut names = Names::new();
    let mut found = Vec::with_capacity(held.len());
    for who in held {
        found.push(sender_name(manager, &mut names, who).await);
    }
    found
}

async fn send_data(
    manager: &mut Mgr,
    ctx: &Ctx,
    thread: &Thread,
    mut message: DataMessage,
    timestamp: u64,
) -> Result<(), String> {
    if let Thread::Group(key) = thread {
        message.group_v2 = Some(group_context(manager, key).await);
    }
    deliver(
        manager,
        ctx,
        thread,
        ContentBody::DataMessage(message),
        timestamp,
    )
    .await
}

/// Replaces the text of our message sent at `target`.
async fn send_edit(
    manager: &mut Mgr,
    ctx: &Ctx,
    thread: &Thread,
    target: u64,
    mut message: DataMessage,
    timestamp: u64,
) -> Result<(), String> {
    if let Thread::Group(key) = thread {
        message.group_v2 = Some(group_context(manager, key).await);
    }
    let edit = ContentBody::EditMessage(EditMessage {
        target_sent_timestamp: Some(target),
        data_message: Some(message),
    });
    deliver(manager, ctx, thread, edit, timestamp).await
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
    let name = crate::format::name(&name);
    names.insert(id, name.clone());
    name
}

/// Receives messages for the rest of the session, reconnecting with exponential backoff
/// whenever the connection drops (sleep, network changes, server timeouts), and at once when
/// the computer wakes from sleep: the old connection may look alive for minutes until a
/// keepalive fails, and a pending retry may be up to a minute away.
/// Notifies `removed` (and stops) if Signal no longer accepts this device.
async fn receive(
    mut manager: Mgr,
    ctx: Ctx,
    avatars: AvatarFetcher,
    removed: Rc<tokio::sync::Notify>,
) {
    const MAX_BACKOFF: Duration = Duration::from_secs(60);
    let mut names = Names::new();
    let mut resets = Resets::new();
    let mut backoff = Duration::from_secs(1);
    let woke = Rc::new(tokio::sync::Notify::new());
    let watching = tokio::task::spawn_local(watch_wakeups(woke.clone()));
    // Stops watching when receiving stops.
    let _watching = AbortOnDrop(watching);
    loop {
        let mut reconnect_now = false;
        match manager.receive_messages().await {
            Ok(messages) => {
                pin_mut!(messages);
                loop {
                    // Only between messages, so one is never handled halfway.
                    let received = tokio::select! {
                        received = messages.next() => received,
                        () = woke.notified() => {
                            info!("woke from sleep; reconnecting to Signal");
                            reconnect_now = true;
                            break;
                        }
                    };
                    let Some(received) = received else {
                        info!("connection to Signal closed");
                        break;
                    };
                    // Something came through, so the connection works again.
                    backoff = Duration::from_secs(1);
                    if !handle_received(&manager, &ctx, &avatars, &mut names, &mut resets, received)
                        .await
                    {
                        return;
                    }
                }
            }
            Err(error) if removed_from_account(&error) => {
                warn!(%error, "Signal no longer accepts this device");
                removed.notify_one();
                return;
            }
            Err(error) => warn!(%error, "failed to connect to Signal"),
        }
        if !ctx.events.send(Event::Reconnecting) {
            return;
        }
        if reconnect_now {
            backoff = Duration::from_secs(1);
            continue;
        }
        tokio::select! {
            () = tokio::time::sleep(backoff) => backoff = (backoff * 2).min(MAX_BACKOFF),
            () = woke.notified() => {
                info!("woke from sleep; reconnecting to Signal");
                backoff = Duration::from_secs(1);
            }
        }
    }
}

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Notifies `woke` each time the computer wakes from sleep, as logind announces it. Ends
/// quietly where there is no logind (the backoff still reconnects, just later).
#[cfg(all(unix, not(target_os = "macos")))]
async fn watch_wakeups(woke: Rc<tokio::sync::Notify>) {
    #[zbus::proxy(
        interface = "org.freedesktop.login1.Manager",
        default_service = "org.freedesktop.login1",
        default_path = "/org/freedesktop/login1"
    )]
    trait Login {
        /// `true` before sleeping, `false` after waking.
        #[zbus(signal)]
        fn prepare_for_sleep(&self, start: bool) -> zbus::Result<()>;
    }

    let watch = async {
        let connection = zbus::Connection::system().await?;
        let mut signals = LoginProxy::new(&connection)
            .await?
            .receive_prepare_for_sleep()
            .await?;
        while let Some(signal) = signals.next().await {
            if !signal.args()?.start {
                woke.notify_one();
            }
        }
        zbus::Result::Ok(())
    };
    if let Err(error) = watch.await {
        debug!(%error, "can't tell when the computer wakes from sleep");
    }
}

/// Elsewhere only the backoff reconnects after sleep.
#[cfg(not(all(unix, not(target_os = "macos"))))]
async fn watch_wakeups(_: Rc<tokio::sync::Notify>) {}

/// Returns `false` once the UI has gone away.
async fn handle_received(
    manager: &Mgr,
    ctx: &Ctx,
    avatars: &AvatarFetcher,
    names: &mut Names,
    resets: &mut Resets,
    received: Received,
) -> bool {
    let events = &ctx.events;
    match received {
        Received::QueueEmpty => events.send(Event::Synced),
        Received::Contacts => {
            names.clear();
            send_conversations(manager, ctx).await;
            // A newly linked device first learns its contacts here.
            avatars.request();
            true
        }
        Received::DecryptionError(sender) => {
            warn!(sender = %sender.service_id_string(), "decryption error");
            // Nothing from blocked people, as for messages that do decrypt: no notice, and no
            // session reset (which is a message to them, and says we are here).
            if is_blocked(ctx, &conversation_id(&Thread::Contact(sender))).await {
                debug!("from a blocked contact; ignored");
                return true;
            }
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
            let conversation = ensure_conversation(manager, ctx, Thread::Contact(sender)).await;
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
        Received::Content(content) => handle_content(manager, ctx, names, &content).await,
    }
}

/// Returns `false` once the UI has gone away.
async fn handle_content(manager: &Mgr, ctx: &Ctx, names: &mut Names, content: &Content) -> bool {
    let events = &ctx.events;
    // Sync messages speak for our account: what our other devices sent, read, blocked or
    // accepted. Only our own devices may say so (the sender is authenticated by the session);
    // from anyone else they are forged, and presage doesn't check (as Signal's clients do).
    if !from_our_account(own_id(manager), content) {
        warn!(
            sender = %content.metadata.sender.service_id_string(),
            "dropped a sync message from another account"
        );
        return true;
    }
    // From someone whose safety number changed: the store noted it while decrypting.
    if content.metadata.sender != own_id(manager) {
        announce_identity_change(manager, ctx, names, content.metadata.sender).await;
        refresh_trust(manager, ctx).await;
    }
    match &content.body {
        ContentBody::ReceiptMessage(receipt) => return on_receipt(ctx, receipt).await,
        // Typing in a group names the group differently from other messages.
        ContentBody::TypingMessage(typing) => {
            return on_typing(manager, ctx, names, content, typing).await;
        }
        ContentBody::SynchronizeMessage(sync) if !sync.read.is_empty() => {
            on_read_elsewhere(manager, ctx, &sync.read).await;
        }
        ContentBody::StoryMessage(story) => {
            let author = content.metadata.sender;
            on_story(manager, ctx, author, content.timestamp(), story, false).await;
            return true;
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content:
                Some(SyncContent::Sent(Sent {
                    story_message: Some(story),
                    timestamp,
                    ..
                })),
            ..
        }) => {
            let at = timestamp.unwrap_or_else(|| content.timestamp());
            on_story(manager, ctx, own_id(manager), at, story, true).await;
            return true;
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Verified(verified)),
            ..
        }) => {
            on_verified_elsewhere(manager, ctx, verified).await;
            return true;
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Blocked(blocked)),
            ..
        }) => {
            on_blocked_elsewhere(manager, ctx, blocked).await;
            return true;
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::MessageRequestResponse(answer)),
            ..
        }) => {
            on_answered_elsewhere(manager, ctx, answer).await;
            return true;
        }
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::FetchLatest(latest)),
            ..
        }) => {
            if latest.r#type() == sync_message::fetch_latest::Type::StorageManifest {
                ctx.storage_changed.notify_one();
            }
            return true;
        }
        // The storage key arrives with it (presage has kept it by now), so the storage
        // service can be read: `follow_storage` may be waiting for exactly this.
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Keys(_)),
            ..
        }) => {
            ctx.storage_changed.notify_one();
            return true;
        }
        _ => {}
    }
    let Ok(thread) = Thread::try_from(content) else {
        return true;
    };
    // A message for a group we aren't in (presage fetches a group from the server the first
    // time a message names it, which fails for one we aren't a member of): a made-up master
    // key, or a group we were removed from. Signal's clients drop these, so that anyone able
    // to write to us can't fill the chat list with groups.
    let known = ctx.threads.borrow().contains_key(&conversation_id(&thread));
    if let Thread::Group(key) = &thread
        && !known
        && !matches!(manager.store().group(*key).await, Ok(Some(_)))
    {
        debug!(
            sender = %content.metadata.sender.service_id_string(),
            "dropped a message for a group we aren't in"
        );
        return true;
    }
    let id = ensure_conversation(manager, ctx, thread.clone()).await;
    // presage has taken over a group change by now; show the group as it is.
    if let Thread::Group(key) = thread
        && let Some(context) = data_of(content)
            .and_then(|(data, _)| data.group_v2.as_ref())
            .filter(|context| context.group_change.is_some())
    {
        forget_membership(manager, key, context).await;
        refresh_group(manager, ctx, key).await;
    }
    // Nothing new from blocked chats (what we send from another device still counts).
    if !is_outgoing(manager, content) && is_blocked(ctx, &id).await {
        return true;
    }
    if let Some((target, reaction, remove)) = to_reaction(manager, names, content).await {
        return events.send(Event::Reaction {
            conversation: id,
            target,
            reaction,
            remove,
        });
    }
    let Some((data, edit)) = data_of(content) else {
        return true;
    };
    let outgoing = is_outgoing(manager, content);
    let author = author_of(manager, content);
    if let Some(Delete {
        target_sent_timestamp: Some(timestamp),
    }) = data.delete
    {
        let target = MessageRef {
            author_id: author.service_id_string(),
            timestamp,
        };
        if let Err(error) = ctx.tables.record_deleted(&id, &target).await {
            warn!(%error, "failed to remember a deletion");
        }
        return events.send(Event::Deleted {
            conversation: id,
            target,
        });
    }
    let Some(message) = to_chat_message(manager, names, id.clone(), content).await else {
        return true;
    };
    if outgoing {
        // Sent from another device: receipts name only the time, so remember where it went.
        if let Err(error) = ctx
            .tables
            .set_delivery(&id, message.timestamp, Delivery::Sent)
            .await
        {
            warn!(%error, "failed to remember a sent message");
        }
        if let ContentBody::SynchronizeMessage(SyncMessage {
            content:
                Some(SyncContent::Sent(Sent {
                    expiration_start_timestamp: Some(start),
                    ..
                })),
            ..
        }) = &content.body
        {
            let timers = [(message.reference(), data.expire_timer.unwrap_or(0))];
            start_timers(ctx, &id, &timers, *start).await;
        }
    }
    // The store holds only the new text now; the state still knows the one before.
    if edit {
        let target = message.reference();
        return events.send(Event::Edited {
            conversation: id,
            target,
            message,
        });
    }
    // A new message brings an archived conversation back, unless it is muted (as in Signal).
    if !outgoing {
        match ctx.tables.unarchive_unless_muted(&id, now_millis()).await {
            Ok(Some(settings)) => {
                events.send(Event::Settings {
                    conversation: id.clone(),
                    settings,
                });
            }
            Ok(None) => {}
            Err(error) => warn!(%error, "failed to unarchive a conversation"),
        }
    }
    events.send(Event::Message(message))
}

async fn is_blocked(ctx: &Ctx, id: &ConversationId) -> bool {
    ctx.tables
        .settings(id)
        .await
        .ok()
        .flatten()
        .is_some_and(|s| s.blocked)
}

/// Whether `id` is a message request not answered yet. Only known once the phone's settings
/// (which say who was accepted) have been read.
async fn is_unanswered_request(ctx: &Ctx, id: &ConversationId) -> bool {
    ctx.tables.storage_version().await.unwrap_or(0) > 0
        && !ctx
            .tables
            .settings(id)
            .await
            .ok()
            .flatten()
            .is_some_and(|s| s.accepted)
}

/// The conversation a sync message names by ACI (as text or bytes) or group id.
fn named_conversation(
    ctx: &Ctx,
    aci: Option<&str>,
    aci_binary: Option<&[u8]>,
    group_id: Option<&[u8]>,
) -> Option<ConversationId> {
    let who = aci
        .and_then(ServiceId::parse_from_service_id_string)
        .or_else(|| aci_binary.and_then(ServiceId::parse_from_service_id_binary));
    if let Some(who) = who {
        return Some(ConversationId::contact(&who.service_id_string()));
    }
    let group_id = group_id?;
    let threads = ctx.threads.borrow();
    threads
        .values()
        .find(|t| matches!(t, Thread::Group(key) if group_identifier(key)[..] == *group_id))
        .map(conversation_id)
}

/// The phone's whole list of whom we block: everything else is unblocked.
async fn on_blocked_elsewhere(manager: &Mgr, ctx: &Ctx, blocked: &sync_message::Blocked) {
    let mut now_blocked = std::collections::HashSet::new();
    for aci in &blocked.acis {
        now_blocked.extend(named_conversation(ctx, Some(aci), None, None));
    }
    for aci in &blocked.acis_binary {
        now_blocked.extend(named_conversation(ctx, None, Some(aci), None));
    }
    for group in &blocked.group_ids {
        now_blocked.extend(named_conversation(ctx, None, None, Some(group)));
    }
    let Ok(all) = ctx.tables.conversations().await else {
        return;
    };
    // Those known only by their phone number are listed by it.
    if !blocked.numbers.is_empty() {
        for id in all.keys() {
            if let Some(Thread::Contact(who @ ServiceId::Pni(_))) = thread_of(id)
                && let Some(number) = e164_of(manager, who).await
                && blocked.numbers.contains(&number)
            {
                now_blocked.insert(id.clone());
            }
        }
    }
    let mut changed = Vec::new();
    for (id, mut settings) in all {
        let blocked = now_blocked.remove(&id);
        if settings.blocked != blocked {
            settings.blocked = blocked;
            changed.push((id, settings));
        }
    }
    // Blocked, but nothing remembered about them yet.
    changed.extend(now_blocked.into_iter().map(|id| {
        let settings = ChatSettings {
            blocked: true,
            ..ChatSettings::default()
        };
        (id, settings)
    }));
    if let Err(error) = ctx.tables.save_settings(&changed).await {
        warn!(%error, "failed to remember whom the phone blocks");
        return;
    }
    for (conversation, settings) in changed {
        let _ = ctx.events.send(Event::Settings {
            conversation,
            settings,
        });
    }
}

/// A message request was answered on another device.
async fn on_answered_elsewhere(
    manager: &Mgr,
    ctx: &Ctx,
    answer: &sync_message::MessageRequestResponse,
) {
    use sync_message::message_request_response::Type;
    let Some(id) = named_conversation(
        ctx,
        answer.thread_aci.as_deref(),
        answer.thread_aci_binary.as_deref(),
        answer.group_id.as_deref(),
    ) else {
        return;
    };
    let kind = answer.r#type();
    let block = matches!(
        kind,
        Type::Block | Type::BlockAndDelete | Type::BlockAndSpam
    );
    let delete = matches!(kind, Type::Delete | Type::BlockAndDelete | Type::Spam);
    let settings = ctx
        .tables
        .update(&id, |s| {
            s.accepted |= kind == Type::Accept;
            s.blocked |= block;
        })
        .await;
    if let Ok(settings) = settings {
        let _ = ctx.events.send(Event::Settings {
            conversation: id.clone(),
            settings,
        });
    }
    if delete && let Some(thread) = thread_of(&id) {
        let mut store = manager.store().clone();
        match store.clear_thread(&thread).await {
            Ok(()) => {
                let _ = ctx.events.send(Event::Cleared(id));
            }
            Err(error) => warn!(%error, "failed to delete a chat deleted elsewhere"),
        }
    }
}

/// Our messages got further: delivered to or read on someone's phone.
async fn on_receipt(ctx: &Ctx, receipt: &ReceiptMessage) -> bool {
    use receipt_message::Type;
    let delivery = match receipt.r#type() {
        Type::Delivery => Delivery::Delivered,
        Type::Read | Type::Viewed => Delivery::Read,
    };
    let found = match ctx.tables.conversations_of(&receipt.timestamp).await {
        Ok(found) => found,
        Err(error) => {
            warn!(%error, "failed to look up receipted messages");
            return true;
        }
    };
    if let Err(error) = ctx.tables.set_deliveries(&found, delivery).await {
        warn!(%error, "failed to remember a receipt");
    }
    let mut by_conversation: HashMap<ConversationId, Vec<u64>> = HashMap::new();
    for (conversation, timestamp) in found {
        by_conversation
            .entry(conversation)
            .or_default()
            .push(timestamp);
    }
    by_conversation
        .into_iter()
        .all(|(conversation, timestamps)| {
            ctx.events.send(Event::Delivered {
                conversation,
                timestamps,
                delivery,
            })
        })
}

async fn on_typing(
    manager: &Mgr,
    ctx: &Ctx,
    names: &mut Names,
    content: &Content,
    typing: &TypingMessage,
) -> bool {
    let sender = content.metadata.sender;
    if sender == own_id(manager) {
        return true;
    }
    let thread = match &typing.group_id {
        Some(group_id) => {
            let threads = ctx.threads.borrow();
            let group = threads.values().find(
                |t| matches!(t, Thread::Group(key) if group_identifier(key)[..] == group_id[..]),
            );
            match group {
                Some(thread) => thread.clone(),
                None => return true,
            }
        }
        None => Thread::Contact(sender),
    };
    let conversation = conversation_id(&thread);
    if !ctx.threads.borrow().contains_key(&conversation) || is_blocked(ctx, &conversation).await {
        return true;
    }
    let name = sender_name(manager, names, sender).await;
    ctx.events.send(Event::Typing {
        conversation,
        sender_id: sender.service_id_string(),
        sender: name,
        typing: typing.action() == typing_message::Action::Started,
    })
}

/// Messages were read on another of our devices.
async fn on_read_elsewhere(manager: &Mgr, ctx: &Ctx, reads: &[sync_message::Read]) {
    let mut read: HashMap<ConversationId, Vec<MessageRef>> = HashMap::new();
    for entry in reads {
        let (Some(sender), Some(timestamp)) = (
            entry.parse_sender_aci().map(ServiceId::Aci),
            entry.timestamp,
        ) else {
            continue;
        };
        let Ok(Some(thread)) = manager
            .store()
            .thread_for_sender_and_timestamp(&sender, timestamp)
            .await
        else {
            continue;
        };
        read.entry(conversation_id(&thread))
            .or_default()
            .push(MessageRef {
                author_id: sender.service_id_string(),
                timestamp,
            });
    }
    let now = now_millis();
    for (conversation, messages) in read {
        let newest = messages
            .iter()
            .map(|m| m.timestamp)
            .max()
            .unwrap_or_default();
        match ctx.tables.mark_read(&conversation, newest).await {
            Ok(settings) => {
                let _ = ctx.events.send(Event::Settings {
                    conversation: conversation.clone(),
                    settings,
                });
            }
            Err(error) => warn!(%error, "failed to remember what was read"),
        }
        let timers = stored_timers(manager, &conversation, &messages).await;
        start_timers(ctx, &conversation, &timers, now).await;
    }
}

/// Registers `thread` and announces it to the UI if it's the first we hear of it.
async fn ensure_conversation(manager: &Mgr, ctx: &Ctx, thread: Thread) -> ConversationId {
    let id = conversation_id(&thread);
    if !ctx.threads.borrow().contains_key(&id) {
        let title = manager.thread_title(&thread).await.unwrap_or_default();
        let subtitle = subtitle(manager, &thread).await;
        let timer = expire_timer(manager, &thread).await;
        let group = match &thread {
            Thread::Group(key) => manager
                .store()
                .group(*key)
                .await
                .ok()
                .flatten()
                .map(|group| (*key, group)),
            Thread::Contact(_) => None,
        };
        let mut conversation = match group {
            Some((key, group)) => {
                group_conversation(manager, &ctx.threads, &mut Names::new(), key, group).await
            }
            None => {
                let contact = match thread {
                    Thread::Contact(who) => Some(who),
                    Thread::Group(_) => None,
                };
                let mut conversation = register(&ctx.threads, thread, title, subtitle, timer);
                if let Some(who) = contact {
                    conversation.trust = trust_of(manager, &ctx.tables, who).await;
                    ctx.trust.borrow_mut().insert(who, conversation.trust);
                }
                conversation
            }
        };
        conversation.settings = ctx
            .tables
            .settings(&id)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        info!(title = conversation.title, "new conversation");
        ctx.events.send(Event::Conversations(vec![conversation]));
    }
    id
}

/// `text` made readable: mention placeholders become "@Name", and Signal's style ranges move
/// to its bytes.
async fn resolve_text(
    manager: &Mgr,
    names: &mut Names,
    text: &str,
    ranges: &[BodyRange],
) -> (String, Vec<StyleRange>, Vec<crate::Mention>) {
    use body_range::{AssociatedValue, Style as WireStyle};
    let mut wire = Vec::new();
    let mut mentioned = Vec::new();
    for range in ranges {
        let value = match &range.associated_value {
            Some(AssociatedValue::MentionAci(aci)) => ServiceId::parse_from_service_id_string(aci)
                .map(|who| RangeValue::Mention(who.service_id_string())),
            Some(AssociatedValue::MentionAciBinary(bytes)) => {
                ServiceId::parse_from_service_id_binary(bytes)
                    .map(|who| RangeValue::Mention(who.service_id_string()))
            }
            Some(AssociatedValue::Style(style)) => match WireStyle::try_from(*style) {
                Ok(WireStyle::Bold) => Some(RangeValue::Style(Style::Bold)),
                Ok(WireStyle::Italic) => Some(RangeValue::Style(Style::Italic)),
                Ok(WireStyle::Spoiler) => Some(RangeValue::Style(Style::Spoiler)),
                Ok(WireStyle::Strikethrough) => Some(RangeValue::Style(Style::Strikethrough)),
                Ok(WireStyle::Monospace) => Some(RangeValue::Style(Style::Monospace)),
                Ok(WireStyle::None) | Err(_) => None,
            },
            None => None,
        };
        let Some(value) = value else { continue };
        if let RangeValue::Mention(who) = &value
            && let Some(who) = ServiceId::parse_from_service_id_string(who)
        {
            let name = display_name(manager, names, who).await;
            mentioned.push((range.start(), name));
        }
        wire.push(WireRange {
            start: range.start(),
            length: range.length(),
            value,
        });
    }
    format::decode(text, &wire, &mentioned)
}

/// Signal's body ranges for text we send.
fn body_ranges(ranges: Vec<WireRange>) -> Vec<BodyRange> {
    use body_range::{AssociatedValue, Style as WireStyle};
    ranges
        .into_iter()
        .map(|range| BodyRange {
            start: Some(range.start),
            length: Some(range.length),
            associated_value: Some(match range.value {
                RangeValue::Mention(aci) => AssociatedValue::MentionAci(aci),
                RangeValue::Style(style) => AssociatedValue::Style(match style {
                    Style::Bold => WireStyle::Bold,
                    Style::Italic => WireStyle::Italic,
                    Style::Spoiler => WireStyle::Spoiler,
                    Style::Strikethrough => WireStyle::Strikethrough,
                    Style::Monospace => WireStyle::Monospace,
                } as i32),
            }),
        })
        .collect()
}

/// The data message a content carries: from someone, from us (synced from another device), or
/// an edit of either; `true` for an edit.
fn data_of(content: &Content) -> Option<(&DataMessage, bool)> {
    match &content.body {
        ContentBody::DataMessage(data) => Some((data, false)),
        ContentBody::EditMessage(EditMessage {
            data_message: Some(data),
            ..
        }) => Some((data, true)),
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Sent(sent)),
            ..
        }) => match sent {
            Sent {
                message: Some(data),
                ..
            } => Some((data, false)),
            Sent {
                edit_message:
                    Some(EditMessage {
                        data_message: Some(data),
                        ..
                    }),
                ..
            } => Some((data, true)),
            _ => None,
        },
        _ => None,
    }
}

/// The kind of note a stored content is about a contact's safety number, if it is one: a
/// `Verified` sync message saved in their thread, without the contact real ones from our own
/// devices name. The store saves one with [`State::Default`] when their key changes; b33p saves
/// one with [`State::Verified`] or [`State::Unverified`] when we mark it verified or clear that
/// (here or on another device).
///
/// [`State::Default`]: presage::proto::verified::State::Default
/// [`State::Verified`]: presage::proto::verified::State::Verified
/// [`State::Unverified`]: presage::proto::verified::State::Unverified
fn identity_note(content: &Content) -> Option<presage::proto::verified::State> {
    match &content.body {
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Verified(verified)),
            ..
        }) if verified.destination_aci.is_none() && verified.destination_aci_binary.is_none() => {
            Some(verified.state())
        }
        _ => None,
    }
}

/// Shows the notes about `contact`'s safety number saved in their chat since `since`: the
/// store saves one while decrypting a message from them or fetching their keys to write to
/// them (before b33p hears of either), b33p when it is verified. Only in their chat if there is
/// one and it isn't blocked; the UI ignores a note it already has.
async fn show_identity_notes(
    manager: &Mgr,
    ctx: &Ctx,
    names: &mut Names,
    contact: ServiceId,
    since: u64,
) {
    let thread = Thread::Contact(contact);
    let id = conversation_id(&thread);
    if contact == own_id(manager)
        || !ctx.threads.borrow().contains_key(&id)
        || is_blocked(ctx, &id).await
    {
        return;
    }
    let Ok(recent) = manager.store().messages(&thread, since..).await else {
        return;
    };
    for content in recent.flatten().filter(|c| identity_note(c).is_some()) {
        if let Some(message) = to_chat_message(manager, names, id.clone(), &content).await {
            let _ = ctx.events.send(Event::Message(message));
        }
    }
}

/// Shows the note of a changed safety number the store may have saved in the last few minutes;
/// see [`show_identity_notes`].
async fn announce_identity_change(manager: &Mgr, ctx: &Ctx, names: &mut Names, contact: ServiceId) {
    let since = crate::now_millis().saturating_sub(5 * 60 * 1000);
    show_identity_notes(manager, ctx, names, contact, since).await;
}

/// `false` for a sync message that didn't come from one of our own devices; see
/// [`handle_content`]. Anything else is fine.
fn from_our_account(own: ServiceId, content: &Content) -> bool {
    !matches!(content.body, ContentBody::SynchronizeMessage(_)) || content.metadata.sender == own
}

/// Ours: sent from here (stored with us as the sender) or synced from another device (which
/// sends it as us too; [`handle_content`] has dropped sync messages from anyone else).
fn is_outgoing(manager: &Mgr, content: &Content) -> bool {
    content.metadata.sender == own_id(manager)
}

/// Who wrote what a content carries: us for what we sent from any device.
fn author_of(manager: &Mgr, content: &Content) -> ServiceId {
    if is_outgoing(manager, content) {
        own_id(manager)
    } else {
        content.metadata.sender
    }
}

/// How the UI names someone: "You" for us.
async fn display_name(manager: &Mgr, names: &mut Names, who: ServiceId) -> String {
    if who == own_id(manager) {
        "You".into()
    } else {
        sender_name(manager, names, who).await
    }
}

/// A reaction, from someone else or (synced from the phone) from us.
async fn to_reaction(
    manager: &Mgr,
    names: &mut Names,
    content: &Content,
) -> Option<(MessageRef, Reaction, bool)> {
    let (data, false) = data_of(content)? else {
        return None;
    };
    let outgoing = is_outgoing(manager, content);
    let reaction = data.reaction.as_ref()?;
    let target = reaction_target(reaction)?;
    let sender = display_name(manager, names, author_of(manager, content)).await;
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
    if let Some(state) = identity_note(content) {
        use presage::proto::verified::State;
        let who = content.metadata.sender;
        let name = display_name(manager, names, who).await;
        return Some(ChatMessage {
            conversation,
            sender_id: who.service_id_string(),
            body: match state {
                State::Default => format!("Your safety number with {name} has changed."),
                State::Verified => {
                    format!("You marked your safety number with {name} as verified.")
                }
                State::Unverified => {
                    format!("You marked your safety number with {name} as unverified.")
                }
            },
            sender: name,
            timestamp: content.timestamp(),
            notice: true,
            ..Default::default()
        });
    }
    let (data, edit) = data_of(content)?;
    let outgoing = is_outgoing(manager, content);
    let author = author_of(manager, content);
    let sender = display_name(manager, names, author).await;
    let (mut body, styles, mentions) = resolve_text(
        manager,
        names,
        data.body.as_deref().unwrap_or_default(),
        &data.body_ranges,
    )
    .await;
    let attachments = attachments::of_message(data);
    let mut timer_change = (data.flags() & Flags::ExpirationTimerUpdate as u32 != 0)
        .then(|| data.expire_timer.unwrap_or(0));
    // Whether it says what changed in the conversation rather than what someone wrote.
    let mut notice = false;
    if let Some(seconds) = timer_change
        && body.is_empty()
    {
        body = crate::timer_notice(&sender, seconds);
        notice = true;
    }
    // A change to the group: what it says.
    if body.is_empty()
        && attachments::of_message(data).is_empty()
        && let Some(context) = &data.group_v2
        && let Some((notice_text, timer)) = group_notice(manager, names, context, &sender).await
    {
        body = notice_text;
        timer_change = timer_change.or(timer);
        notice = true;
    }
    if body.is_empty() && attachments.is_empty() {
        return None;
    }
    let quote = match &data.quote {
        Some(quote) => {
            let author_id = quote.parse_author_aci().map(ServiceId::Aci);
            let author = match author_id {
                Some(author) => display_name(manager, names, author).await,
                None => String::new(),
            };
            let (text, styles, _) = resolve_text(
                manager,
                names,
                quote.text.as_deref().unwrap_or_default(),
                &quote.body_ranges,
            )
            .await;
            Some(Quote {
                author,
                text: format::hide_spoilers(&text, &styles),
                author_id: author_id
                    .map(|id| id.service_id_string())
                    .unwrap_or_default(),
                timestamp: quote.id.unwrap_or_default(),
            })
        }
        None => None,
    };
    Some(ChatMessage {
        conversation,
        sender_id: author.service_id_string(),
        sender,
        body,
        // For an edit, the time of the message it replaces, which identifies it.
        timestamp: content.timestamp(),
        outgoing,
        quote,
        attachments,
        styles,
        mentions,
        expire_timer: data.expire_timer.filter(|&t| t > 0),
        timer_change,
        notice,
        preview: data.preview.first().and_then(|preview| {
            // Only for a link the text has (as Signal's clients require): otherwise a card
            // with a trusted site's title and picture could lead somewhere else entirely.
            let url = preview
                .url
                .clone()
                .filter(|url| url.starts_with("https://"))?;
            let text = data.body.as_deref().unwrap_or_default();
            if !format::links(text).contains(&url.as_str()) {
                debug!("dropped a link preview for a link not in the message");
                return None;
            }
            Some(crate::LinkPreview {
                url,
                title: crate::format::name(preview.title.as_deref().unwrap_or_default()),
                description: preview.description.clone().unwrap_or_default(),
                image: preview
                    .image
                    .as_ref()
                    .map(|pointer| attachments::describe(pointer, false)),
            })
        }),
        edited_at: edit.then(|| data.timestamp.unwrap_or_default()),
        ..Default::default()
    })
}

async fn handle(
    manager: &mut Mgr,
    ctx: &Ctx,
    downloads: &Rc<tokio::sync::Semaphore>,
    command: Command,
) -> Result<(), String> {
    // Chat settings the phone keeps too: write them there, if that is wanted.
    let to_storage = matches!(
        command,
        Command::SetPins(_)
            | Command::Mute { .. }
            | Command::Archive { .. }
            | Command::MarkUnread(_)
            | Command::Block { .. }
            | Command::Respond { .. }
            | Command::Preferences(_)
    );
    let result = handle_command(manager, ctx, downloads, command).await;
    if to_storage && ctx.preferences.get().sync_to_phone {
        ctx.storage_changed.notify_one();
    }
    // Sending may have met a new key of a verified contact.
    refresh_trust(manager, ctx).await;
    result
}

async fn handle_command(
    manager: &mut Mgr,
    ctx: &Ctx,
    downloads: &Rc<tokio::sync::Semaphore>,
    command: Command,
) -> Result<(), String> {
    let servers = manager.registration_data().signal_servers;
    let events = &ctx.events;
    let thread = |id: &ConversationId| {
        ctx.threads
            .borrow()
            .get(id)
            .cloned()
            .ok_or_else(|| "unknown conversation".to_string())
    };
    let db = |error: sqlx::Error| format!("failed to save: {error}");
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
        Command::LoadHistory {
            conversation: id,
            before,
        } => {
            let Ok(thread) = thread(&id) else {
                return Ok(());
            };
            let (contents, older) = history_page(manager, &thread, before).await?;
            let mut names = Names::new();
            let mut messages = Vec::new();
            let mut reactions = Vec::new();
            // Oldest first, so reactions find their message and later ones win.
            for content in contents.iter().rev() {
                if let Some((target, reaction, remove)) =
                    to_reaction(manager, &mut names, content).await
                {
                    if messages.iter().any(|m: &ChatMessage| m.is(&target)) {
                        apply_reaction(&mut messages, &target, reaction, remove);
                    } else {
                        reactions.push((target, reaction, remove));
                    }
                } else if let Some(message) =
                    to_chat_message(manager, &mut names, id.clone(), content).await
                {
                    messages.push(message);
                }
            }
            decorate(ctx, &id, &mut messages).await;
            // The store keeps nothing of messages deleted for everyone; those of this page.
            let own = own_id(manager).service_id_string();
            let page = older.unwrap_or(0)..before.unwrap_or(u64::MAX);
            let deleted = ctx.tables.deleted(&id).await.unwrap_or_default();
            for target in deleted
                .into_iter()
                .filter(|target| page.contains(&target.timestamp))
            {
                let outgoing = target.author_id == own;
                let sender = match ServiceId::parse_from_service_id_string(&target.author_id) {
                    Some(author) => display_name(manager, &mut names, author).await,
                    None => String::new(),
                };
                messages.push(ChatMessage {
                    conversation: id.clone(),
                    sender_id: target.author_id,
                    sender,
                    timestamp: target.timestamp,
                    outgoing,
                    deleted: true,
                    ..Default::default()
                });
            }
            let _ = events.send(Event::History {
                conversation: id,
                messages,
                reactions,
                older,
            });
        }
        Command::React {
            conversation,
            target,
            emoji,
            remove,
        } => {
            let thread = thread(&conversation)?;
            let message = DataMessage {
                reaction: Some(presage::proto::data_message::Reaction {
                    emoji: Some(emoji.clone()),
                    remove: Some(remove),
                    target_author_aci: Some(target.author_id.clone()),
                    target_author_aci_binary: aci_binary(&target.author_id),
                    target_sent_timestamp: Some(target.timestamp),
                }),
                ..Default::default()
            };
            send_data(manager, ctx, &thread, message, now_millis()).await?;
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
            timestamp,
            body,
            quote,
            files,
            styles,
            mentions,
        } => {
            let sent = async {
                let pointers = attachments::upload(manager, &files).await?;
                let preview = if ctx.preferences.get().link_previews {
                    link_preview(manager, &body).await
                } else {
                    None
                };
                let outgoing = Outgoing {
                    body,
                    styles,
                    mentions,
                    quote,
                    pointers,
                    preview,
                    sticker: None,
                };
                send_new(manager, ctx, conversation.clone(), outgoing, timestamp).await
            };
            if let Err(error) = sent.await {
                let _ = ctx.events.send(Event::SendFailed {
                    conversation,
                    timestamp,
                });
                return Err(error);
            }
        }
        Command::ViewStory(story) => {
            ctx.tables.story_viewed(&story).await.map_err(db)?;
            send_stories(manager, ctx).await;
            let receipts = ctx.preferences.get().read_receipts;
            tokio::task::spawn_local(send_viewed(manager.clone(), story, receipts));
        }
        Command::SendStory {
            conversation,
            story,
        } => {
            let Thread::Group(key) = thread(&conversation)? else {
                return Err("stories go to groups here".into());
            };
            let timestamp = now_millis();
            let mut stored = crate::local::StoredStory {
                author: own_id(manager).service_id_string(),
                timestamp,
                conversation: conversation.clone(),
                viewed: true,
                outgoing: true,
                ..Default::default()
            };
            let attachment = match story {
                crate::NewStory::Text(text) => {
                    stored.text = text.clone();
                    stored.foreground = STORY_FOREGROUND;
                    stored.background = STORY_BACKGROUND;
                    presage::proto::story_message::Attachment::TextAttachment(
                        presage::proto::TextAttachment {
                            text: Some(text),
                            text_style: Some(
                                presage::proto::text_attachment::Style::Regular as i32,
                            ),
                            text_foreground_color: Some(STORY_FOREGROUND),
                            background: Some(presage::proto::text_attachment::Background::Color(
                                STORY_BACKGROUND,
                            )),
                            ..Default::default()
                        },
                    )
                }
                crate::NewStory::Picture(path) => {
                    let pointer = attachments::upload(manager, &[path])
                        .await?
                        .pop()
                        .ok_or("nothing uploaded")?;
                    stored.pointer = Some(pointer.encode_to_vec());
                    presage::proto::story_message::Attachment::FileAttachment(pointer)
                }
            };
            let message = presage::proto::StoryMessage {
                group: Some(group_context(manager, &key).await),
                attachment: Some(attachment),
                allows_replies: Some(true),
                ..Default::default()
            };
            manager
                .send_message_to_group(&key, ContentBody::StoryMessage(message.clone()), timestamp)
                .await
                .map_err(|e| format!("failed to post the story: {e}"))?;
            ctx.tables.add_story(&stored).await.map_err(db)?;
            send_stories(manager, ctx).await;
            // Sending tells our other devices of data messages only; this one they hear of here.
            let sync = SyncMessage {
                content: Some(SyncContent::Sent(Sent {
                    timestamp: Some(timestamp),
                    story_message: Some(message),
                    ..Default::default()
                })),
                ..Default::default()
            };
            send_sync(manager, sync).await;
        }
        Command::CreateGroup { title, members } => {
            let members = members
                .iter()
                .map(|id| member_aci(id))
                .collect::<Option<Vec<_>>>()
                .ok_or(PHONE_NUMBER_ONLY)?;
            let (key, timestamp) = manager
                .create_group(title.trim(), &members)
                .await
                .map_err(|e| format!("failed to create the group: {e}"))?;
            let thread = Thread::Group(key);
            let id = ensure_conversation(manager, ctx, thread.clone()).await;
            // Ours, so not a request.
            ctx.tables
                .update(&id, |s| s.accepted = true)
                .await
                .map_err(db)?;
            refresh_group(manager, ctx, key).await;
            show_stored(manager, ctx, &id, &thread, timestamp).await;
            let _ = events.send(Event::GroupCreated(id));
        }
        Command::EditGroup { conversation, edit } => {
            let Thread::Group(key) = thread(&conversation)? else {
                return Err("not a group".into());
            };
            let edit = group_edit(edit).ok_or(PHONE_NUMBER_ONLY)?;
            if matches!(edit, presage::manager::GroupEdit::Leave) {
                only_admin_check(manager, key).await?;
            }
            let timestamp = manager
                .change_group(&key, edit)
                .await
                .map_err(|e| format!("failed to change the group: {e}"))?;
            refresh_group(manager, ctx, key).await;
            show_stored(manager, ctx, &conversation, &Thread::Group(key), timestamp).await;
        }
        Command::LoadStickers => {
            let packs = sticker_packs(manager).await;
            let _ = events.send(Event::StickerPacks(packs));
        }
        Command::SendSticker {
            conversation,
            pack,
            sticker,
        } => {
            let sticker = sticker_message(manager, &pack, sticker).await?;
            let outgoing = Outgoing {
                body: String::new(),
                styles: Vec::new(),
                mentions: Vec::new(),
                quote: None,
                pointers: Vec::new(),
                preview: None,
                sticker: Some(sticker),
            };
            send_new(manager, ctx, conversation, outgoing, now_millis()).await?;
        }
        Command::Block {
            conversation,
            blocked,
        } => {
            ctx.tables
                .update(&conversation, |s| s.blocked = blocked)
                .await
                .map_err(db)?;
            send_blocked(manager.clone(), ctx).await;
        }
        Command::Respond {
            conversation,
            response,
        } => {
            let thread = thread(&conversation)?;
            respond(manager, ctx, &conversation, &thread, response).await?;
        }
        Command::SetVerified {
            conversation,
            verified,
        } => set_verified(manager, ctx, &conversation, verified).await?,
        Command::FindNumber { number, then } => {
            let conversation = find_number(manager, ctx, &number).await?;
            let _ = events.send(Event::Found { conversation, then });
        }
        Command::LoadDetails(conversation) => {
            let Ok(thread) = thread(&conversation) else {
                return Ok(());
            };
            let details = details(manager, &ctx.tables, &thread).await;
            let _ = events.send(Event::Details {
                conversation,
                details,
            });
        }
        Command::LoadMedia(conversation) => {
            let Ok(thread) = thread(&conversation) else {
                return Ok(());
            };
            let messages = media(manager, &thread, &conversation).await?;
            let _ = events.send(Event::Media {
                conversation,
                messages,
            });
        }
        Command::SearchChat {
            conversation,
            query,
        } => {
            let Ok(thread) = thread(&conversation) else {
                return Ok(());
            };
            let messages = search(manager, &thread, &conversation, &query).await?;
            let _ = events.send(Event::SearchResults {
                conversation,
                query,
                messages,
            });
        }
        // The attachments are already on the CDN; their pointers work for anyone.
        Command::Forward {
            conversation,
            body,
            styles,
            attachments,
        } => {
            let pointers = attachments
                .iter()
                .filter_map(attachments::pointer)
                .collect();
            let outgoing = Outgoing {
                body,
                styles,
                mentions: Vec::new(),
                quote: None,
                pointers,
                preview: None,
                sticker: None,
            };
            send_new(manager, ctx, conversation, outgoing, now_millis()).await?;
        }
        Command::Edit {
            conversation,
            target,
            body,
            styles,
            mentions,
        } => {
            let thread = thread(&conversation)?;
            // An edit replaces the whole message, so all but the text comes from the message
            // as sent: its attachments, quote, sticker and timer stay (receivers show what
            // the edit carries, and nothing else).
            let stored = manager
                .store()
                .message(&thread, target.timestamp)
                .await
                .ok()
                .flatten();
            let original = stored
                .as_ref()
                .and_then(data_of)
                .map(|(data, _)| data.clone())
                .unwrap_or_default();
            // Kept as the earlier version.
            let previous = original.body.clone().unwrap_or_default();
            let (text, ranges) = format::encode(&body, &styles, &mentions);
            let timestamp = now_millis();
            let mut data = DataMessage {
                body: (!text.is_empty()).then_some(text),
                body_ranges: body_ranges(ranges),
                timestamp: Some(timestamp),
                ..original
            };
            // The preview follows the text: gone with its link, made anew for a new one.
            let links = format::links(&body);
            data.preview.retain(|preview| {
                preview
                    .url
                    .as_deref()
                    .is_some_and(|url| links.contains(&url))
            });
            if data.preview.is_empty()
                && ctx.preferences.get().link_previews
                && let Some((sent, _)) = link_preview(manager, &body).await
            {
                data.preview.push(sent);
            }
            send_edit(
                manager,
                ctx,
                &thread,
                target.timestamp,
                data.clone(),
                timestamp,
            )
            .await?;
            // For after a restart; the state keeps the text before by itself.
            ctx.tables
                .record_edit(&conversation, &target, timestamp, &previous)
                .await
                .map_err(db)?;
            // Shown as it will be read back: the edit in the message's place.
            let edited = stored.map(|mut content| {
                content.body = ContentBody::EditMessage(EditMessage {
                    target_sent_timestamp: Some(target.timestamp),
                    data_message: Some(data),
                });
                content
            });
            let message = match edited {
                Some(content) => {
                    to_chat_message(manager, &mut Names::new(), conversation.clone(), &content)
                        .await
                }
                None => None,
            };
            let message = message.unwrap_or_else(|| ChatMessage {
                conversation: conversation.clone(),
                sender_id: target.author_id.clone(),
                sender: "You".into(),
                body,
                timestamp: target.timestamp,
                outgoing: true,
                styles,
                edited_at: Some(timestamp),
                ..Default::default()
            });
            let _ = events.send(Event::Edited {
                conversation,
                target,
                message,
            });
        }
        Command::Delete {
            conversation,
            target,
            for_everyone: true,
        } => {
            let thread = thread(&conversation)?;
            let message = DataMessage {
                delete: Some(Delete {
                    target_sent_timestamp: Some(target.timestamp),
                }),
                ..Default::default()
            };
            send_data(manager, ctx, &thread, message, now_millis()).await?;
            ctx.tables
                .record_deleted(&conversation, &target)
                .await
                .map_err(db)?;
        }
        Command::Delete {
            conversation,
            target,
            for_everyone: false,
        } => {
            let thread = thread(&conversation)?;
            let mut store = manager.store().clone();
            store
                .delete_message(&thread, target.timestamp)
                .await
                .map_err(|e| format!("failed to delete: {e}"))?;
        }
        Command::Typing {
            conversation,
            typing,
        } => {
            if !ctx.preferences.get().typing_indicators {
                return Ok(());
            }
            let thread = thread(&conversation)?;
            // In the background: it may take a moment, and nobody waits for it.
            tokio::task::spawn_local(send_typing(manager.clone(), thread, typing));
        }
        Command::MarkRead {
            conversation,
            messages,
            expiring,
        } => {
            let newest = messages
                .iter()
                .map(|m| m.timestamp)
                .max()
                .unwrap_or_default();
            ctx.tables
                .mark_read(&conversation, newest)
                .await
                .map_err(db)?;
            if messages.is_empty() {
                return Ok(());
            }
            start_timers(ctx, &conversation, &expiring, now_millis()).await;
            // Not to unanswered message requests: they'd tell a stranger we read it.
            let receipts = ctx.preferences.get().read_receipts
                && !is_unanswered_request(ctx, &conversation).await;
            tokio::task::spawn_local(send_read(manager.clone(), messages, receipts));
        }
        Command::MarkUnread(conversation) => {
            ctx.tables
                .update(&conversation, |s| s.marked_unread = true)
                .await
                .map_err(db)?;
        }
        Command::SetPins(order) => ctx.tables.set_pins(&order).await.map_err(db)?,
        Command::Mute {
            conversation,
            until,
        } => {
            ctx.tables
                .update(&conversation, |s| s.muted_until = until)
                .await
                .map_err(db)?;
        }
        Command::Archive {
            conversation,
            archived,
        } => {
            ctx.tables
                .update(&conversation, |s| s.set_archived(archived))
                .await
                .map_err(db)?;
        }
        Command::SetTimer {
            conversation,
            seconds,
        } => {
            let thread = thread(&conversation)?;
            if let Thread::Group(key) = thread {
                let edit = presage::manager::GroupEdit::Timer(seconds);
                let timestamp = manager
                    .change_group(&key, edit)
                    .await
                    .map_err(|e| format!("failed to change the group: {e}"))?;
                refresh_group(manager, ctx, key).await;
                show_stored(manager, ctx, &conversation, &thread, timestamp).await;
                return Ok(());
            }
            let timestamp = now_millis();
            let message = DataMessage {
                flags: Some(Flags::ExpirationTimerUpdate as u32),
                expire_timer: Some(seconds),
                ..Default::default()
            };
            send_data(manager, ctx, &thread, message, timestamp).await?;
            let _ = events.send(Event::Message(ChatMessage {
                conversation,
                sender_id: own_id(manager).service_id_string(),
                sender: "You".into(),
                body: crate::timer_notice("You", seconds),
                notice: true,
                timestamp,
                outgoing: true,
                expire_timer: (seconds > 0).then_some(seconds),
                timer_change: Some(seconds),
                ..Default::default()
            }));
        }
        Command::Preferences(preferences) => ctx.preferences.set(preferences),
    }
    Ok(())
}

/// What the details page shows about a thread. Whatever can't be found is left out.
async fn details(manager: &mut Mgr, tables: &LocalStore, thread: &Thread) -> crate::Details {
    let mut details = crate::Details::default();
    match thread {
        Thread::Contact(who) => {
            if let Ok(Some(contact)) = manager.store().contact_by_id(who).await {
                details.phone = contact.phone_number.as_ref().map(display_number);
            }
            if *who != own_id(manager) {
                details.safety_number = safety_number(manager, *who).await;
                details.trust = trust_of(manager, tables, *who).await;
            }
            if let (ServiceId::Aci(aci), Ok(Some(key))) =
                (who, manager.store().profile_key(who).await)
            {
                match manager.retrieve_profile_by_uuid(*aci, key).await {
                    Ok(profile) => {
                        let about = [profile.about_emoji, profile.about]
                            .into_iter()
                            .flatten()
                            .filter(|s| !s.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ");
                        details.about = (!about.is_empty()).then_some(about);
                    }
                    Err(error) => debug!(%error, "couldn't fetch a profile"),
                }
            }
        }
        Thread::Group(key) => {
            if let Ok(Some(group)) = manager.store().group(*key).await {
                details.description = group.description.clone().filter(|d| !d.is_empty());
                let mut names = Names::new();
                for member in &group.members {
                    let who = ServiceId::Aci(member.aci);
                    let person = crate::Person {
                        service_id: who.service_id_string(),
                        name: display_name(manager, &mut names, who).await,
                    };
                    let admin =
                        member.role == presage::libsignal_service::groups_v2::Role::Administrator;
                    details.members.push((person, admin));
                }
                // Admins first, then by name.
                details
                    .members
                    .sort_by_cached_key(|(p, admin)| (!admin, p.name.to_lowercase()));
            }
        }
    }
    details
}

/// `who`'s identity key, as their messages and sessions here have it, if there is one yet.
async fn identity_of(
    manager: &Mgr,
    who: ServiceId,
) -> Option<presage::libsignal_service::protocol::IdentityKey> {
    use presage::libsignal_service::protocol::{DeviceId, IdentityKeyStore, ProtocolAddress};
    use presage::store::Store as _;
    let address = ProtocolAddress::new(who.service_id_string(), DeviceId::new(1).ok()?);
    manager
        .store()
        .aci_protocol_store()
        .get_identity(&address)
        .await
        .ok()?
}

/// The key `who` has now, as far as is known here: the one the store refused for sending while
/// their verified one is pinned (it came with their pre-key bundle; see [`hold_changed`]), else
/// the one their messages and sessions have. The safety number shown, and the key verified or
/// accepted, is of this one.
async fn key_shown(
    manager: &Mgr,
    who: ServiceId,
) -> Option<presage::libsignal_service::protocol::IdentityKey> {
    match manager.store().offered_identity(&who).await {
        Ok(Some(offered)) => Some(offered),
        Ok(None) => identity_of(manager, who).await,
        Err(error) => {
            warn!(%error, "failed to read the key the store refused for sending");
            identity_of(manager, who).await
        }
    }
}

/// How far the safety number with `who` is trusted: verified for the key they have now, or
/// verified before and changed since.
async fn trust_of(manager: &Mgr, tables: &LocalStore, who: ServiceId) -> crate::Trust {
    let verification = tables.verification(&who.service_id_string()).await;
    trust_given(manager, who, verification.ok().flatten().as_ref()).await
}

/// [`trust_of`] with what is kept about `who`'s verification already read; their key is only
/// looked up if there is something to compare it with.
async fn trust_given(
    manager: &Mgr,
    who: ServiceId,
    verification: Option<&crate::local::Verification>,
) -> crate::Trust {
    use presage::proto::verified::State;
    if !verification.is_some_and(|v| v.state != State::Default) {
        return crate::Trust::Unverified;
    }
    trust(verification, identity_of(manager, who).await.as_ref())
}

/// How far a safety number is trusted, given what is kept about its verification and the key
/// the contact has now: verified only for that key; changed if an earlier one was, or if the
/// change was seen on another device and not confirmed there (Signal's "unverified").
fn trust(
    verification: Option<&crate::local::Verification>,
    key: Option<&presage::libsignal_service::protocol::IdentityKey>,
) -> crate::Trust {
    use presage::proto::verified::State;
    match (verification, key) {
        (Some(v), Some(key)) => match v.state {
            State::Default => crate::Trust::Unverified,
            State::Verified if *v.identity_key == *key.serialize() => crate::Trust::Verified,
            State::Verified | State::Unverified => crate::Trust::Changed,
        },
        // Nothing kept, or no key yet: nothing to compare, nothing changed.
        _ => crate::Trust::Unverified,
    }
}

/// Tells the UI how far `who`'s safety number is trusted, if that isn't what it was last told.
fn publish_trust(ctx: &Ctx, who: ServiceId, trust: crate::Trust) {
    if ctx.trust.borrow_mut().insert(who, trust) != Some(trust) {
        let _ = ctx.events.send(Event::Trust {
            conversation: conversation_id(&Thread::Contact(who)),
            trust,
        });
    }
}

/// Works out again how far each contact with a kept verification (few) is trusted, and tells the
/// UI of those that changed. Their key can change in any message sent or received (the store
/// notices it first), so this runs after each.
async fn refresh_trust(manager: &Mgr, ctx: &Ctx) {
    use presage::proto::verified::State;
    let Ok(all) = ctx.tables.verifications().await else {
        return;
    };
    for (aci, verification) in all.iter().filter(|(_, v)| v.state != State::Default) {
        if let Some(who) = ServiceId::parse_from_service_id_string(aci) {
            let trust = trust_given(manager, who, Some(verification)).await;
            publish_trust(ctx, who, trust);
        }
    }
}

/// Where a verification comes from: marked here, or taken over from another device's message
/// or the storage service (`quiet` on the first read, which brings what was set before).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verifier {
    Here,
    Elsewhere { quiet: bool },
}

/// Marks the safety number with a contact verified, or clears that (which also accepts one that
/// changed): here first, with a note in the chat; then, as Signal's apps do, a null message to
/// them (so that what the server sees is an ordinary message to them) and the change to our
/// other devices with the same padding. Not reaching our devices is an error to show, but it
/// is done here: accepting a change is ours to decide, and writing to them shouldn't wait.
async fn set_verified(
    manager: &mut Mgr,
    ctx: &Ctx,
    conversation: &ConversationId,
    verified: bool,
) -> Result<(), String> {
    use presage::libsignal_service::content::NullMessage;
    use presage::proto::verified::State;
    let thread = ctx.threads.borrow().get(conversation).cloned();
    let who = match thread {
        Some(Thread::Contact(who @ ServiceId::Aci(_))) if who != own_id(manager) => who,
        _ => return Err("only a contact has a safety number to verify".into()),
    };
    let current = identity_of(manager, who).await;
    let key = key_shown(manager, who)
        .await
        .ok_or("there is no safety number with them yet")?;
    if current.as_ref() != Some(&key) {
        take_over(manager, ctx, who, &key).await?;
    }
    let state = if verified {
        State::Verified
    } else {
        State::Default
    };
    remember_verification(manager, ctx, who, &key, state, Verifier::Here).await?;
    let padding = NullMessage::generate(&mut rand::rng()).padding;
    let timestamp = now_millis();
    if let Err(error) = manager
        .send_message(
            who,
            ContentBody::NullMessage(NullMessage {
                padding: padding.clone(),
            }),
            timestamp,
        )
        .await
    {
        warn!(%error, "failed to send the null message that goes with a verification");
    }
    let sync = verified_sync(who, &key, verified, padding);
    let own = own_id(manager);
    manager
        .send_message(own, ContentBody::SynchronizeMessage(sync), timestamp)
        .await
        .map_err(|e| format!("done here, but your other devices couldn't be told: {e}"))?;
    Ok(())
}

/// Takes `key`, which the store refused for sending to `who` and kept as offered, as theirs:
/// saved as their identity (the store notes the change in their chat; shown at once) and their
/// sessions on the old key dropped, as Signal's apps do when a key changes, so that the next
/// message to them sets up a session with the new one.
async fn take_over(
    manager: &Mgr,
    ctx: &Ctx,
    who: ServiceId,
    key: &presage::libsignal_service::protocol::IdentityKey,
) -> Result<(), String> {
    use presage::libsignal_service::protocol::{DeviceId, IdentityKeyStore, ProtocolAddress};
    use presage::libsignal_service::session_store::SessionStoreExt;
    use presage::store::Store as _;
    let address = ProtocolAddress::new(
        who.service_id_string(),
        DeviceId::new(1).expect("a device id"),
    );
    let since = now_millis().saturating_sub(1000);
    let mut store = manager.store().aci_protocol_store();
    store
        .save_identity(&address, key)
        .await
        .map_err(|e| format!("failed to take their new key: {e}"))?;
    if let Err(error) = store.delete_all_sessions(&who).await {
        warn!(%error, "failed to drop the sessions on their old key");
    }
    show_identity_notes(manager, ctx, &mut Names::new(), who, since).await;
    Ok(())
}

/// What tells our other devices that the safety number with `who` (their `key`) is verified,
/// or not anymore: Signal's apps clear it to the default state. `padding` is the null message's
/// sent to them with it.
fn verified_sync(
    who: ServiceId,
    key: &presage::libsignal_service::protocol::IdentityKey,
    verified: bool,
    padding: Option<Vec<u8>>,
) -> SyncMessage {
    use presage::proto::verified::State;
    let state = if verified {
        State::Verified
    } else {
        State::Default
    };
    SyncMessage {
        content: Some(SyncContent::Verified(presage::proto::Verified {
            destination_aci: Some(who.service_id_string()),
            destination_aci_binary: Some(who.service_id_binary()),
            identity_key: Some(key.serialize().to_vec()),
            state: Some(state as i32),
            null_message: padding,
        })),
        ..SyncMessage::with_padding(&mut rand::rng())
    }
}

/// Whom and which key of theirs a verification from another device is about: a contact's ACI
/// and a key that decodes.
fn verified_target(
    verified: &presage::proto::Verified,
) -> Option<(ServiceId, presage::libsignal_service::protocol::IdentityKey)> {
    use presage::libsignal_service::protocol::IdentityKey;
    let who = verified
        .destination_aci_binary
        .as_deref()
        .and_then(ServiceId::parse_from_service_id_binary)
        .or_else(|| {
            verified
                .destination_aci
                .as_deref()
                .and_then(ServiceId::parse_from_service_id_string)
        })
        .filter(|who| matches!(who, ServiceId::Aci(_)))?;
    let key = IdentityKey::decode(verified.identity_key.as_deref()?).ok()?;
    Some((who, key))
}

/// Keeps the verification `state` of `who`'s `key`, notes the change in their chat and shows it,
/// unless it was already so. From another device, a key that isn't the one we have for them is
/// taken over as theirs, as Signal's apps do: the phone compared it.
async fn remember_verification(
    manager: &Mgr,
    ctx: &Ctx,
    who: ServiceId,
    key: &presage::libsignal_service::protocol::IdentityKey,
    state: presage::proto::verified::State,
    verifier: Verifier,
) -> Result<(), String> {
    use presage::libsignal_service::protocol::{DeviceId, IdentityKeyStore, ProtocolAddress};
    use presage::store::Store as _;
    let aci = who.service_id_string();
    let address = ProtocolAddress::new(aci.clone(), DeviceId::new(1).expect("a device id"));
    let mut current = identity_of(manager, who).await;
    if verifier != Verifier::Here && current.as_ref() != Some(key) {
        let mut store = manager.store().aci_protocol_store();
        match store.save_identity(&address, key).await {
            Ok(_) => current = identity_of(manager, who).await,
            Err(error) => warn!(%error, "failed to take over a key verified on another device"),
        }
    }
    let verification = crate::local::Verification {
        identity_key: key.serialize().to_vec(),
        state,
    };
    let db = |error: sqlx::Error| format!("failed to save: {error}");
    let before = ctx.tables.verification(&aci).await.map_err(db)?;
    if before.as_ref() == Some(&verification) {
        return Ok(());
    }
    // A note only for their key as it is here: one for another would contradict what the
    // details page says.
    let quiet = verifier == (Verifier::Elsewhere { quiet: true });
    let note = verification_note(before.as_ref(), &verification)
        .filter(|_| !quiet && current.as_ref() == Some(key));
    ctx.tables
        .set_verification(&aci, &verification)
        .await
        .map_err(db)?;
    pin_verified(manager, who, &verification).await;
    publish_trust(ctx, who, trust(Some(&verification), current.as_ref()));
    if let Some(state) = note {
        let since = now_millis();
        match presage::store::save_trusted_identity_message(manager.store(), &address, *key, state)
            .await
        {
            Ok(()) => show_identity_notes(manager, ctx, &mut Names::new(), who, since).await,
            // The verification itself is kept; only its note is missing.
            Err(error) => warn!(%error, "failed to save the note of a verification"),
        }
    }
    Ok(())
}

/// The note in the chat a change of a verification deserves: having verified the key, or having
/// cleared a verification of it. Nothing for no change, for clearing what wasn't verified, nor
/// for another device seeing their key change (Signal's "unverified": the change notice says so).
fn verification_note(
    before: Option<&crate::local::Verification>,
    after: &crate::local::Verification,
) -> Option<presage::proto::verified::State> {
    use presage::proto::verified::State;
    if before == Some(after) {
        return None;
    }
    match after.state {
        State::Verified => Some(State::Verified),
        State::Default
            if before.is_some_and(|b| {
                b.state == State::Verified && b.identity_key == after.identity_key
            }) =>
        {
            Some(State::Unverified)
        }
        _ => None,
    }
}

/// Keeps the store's pin in step with a kept verification: a verified key is the only one sent
/// to (a new one of theirs is refused for sending, see [`deliver`]); clearing it, or accepting
/// a change, lifts that; a change not confirmed yet keeps it.
async fn pin_verified(manager: &Mgr, who: ServiceId, verification: &crate::local::Verification) {
    use presage::{libsignal_service::protocol::IdentityKey, proto::verified::State};
    let pinned = match verification.state {
        State::Verified => match IdentityKey::decode(&verification.identity_key) {
            Ok(key) => manager.store().pin_identity(&who, &key).await,
            Err(_) => return,
        },
        State::Default => manager.store().unpin_identity(&who).await,
        State::Unverified => return,
    };
    if let Err(error) = pinned {
        warn!(%error, "failed to pin or unpin a verified safety number");
    }
}

/// Holds the chat with `who`, whose identity the store refused for sending: their verified key
/// is pinned, so their new one wasn't taken (the store keeps it as offered, see [`key_shown`],
/// until [`set_verified`] takes it over); keeping the pinned one as "unverified" (changed, not
/// confirmed) says so until they confirm or verify again.
async fn hold_changed(manager: &Mgr, ctx: &Ctx, who: ServiceId) {
    use presage::proto::verified::State;
    let Some(key) = identity_of(manager, who).await else {
        return;
    };
    if let Err(error) =
        remember_verification(manager, ctx, who, &key, State::Unverified, Verifier::Here).await
    {
        warn!(%error, "failed to hold a chat whose safety number changed");
    }
}

/// Our phone (or another device) verified the safety number with a contact, cleared that, or saw
/// it change.
async fn on_verified_elsewhere(manager: &Mgr, ctx: &Ctx, verified: &presage::proto::Verified) {
    let Some((who, key)) = verified_target(verified) else {
        warn!("a verification from another device without whom or which key it is about");
        return;
    };
    let verifier = Verifier::Elsewhere { quiet: false };
    if let Err(error) =
        remember_verification(manager, ctx, who, &key, verified.state(), verifier).await
    {
        warn!(%error, "failed to keep a verification from another device");
    }
}

/// A phone number as typed: international ("+49 151 …"), or in the way of `own`'s country
/// ("0151 …"); `None` unless it is a valid number.
fn parse_number(
    text: &str,
    own: &presage::libsignal_service::prelude::phonenumber::PhoneNumber,
) -> Option<presage::libsignal_service::prelude::phonenumber::PhoneNumber> {
    use presage::libsignal_service::prelude::phonenumber;
    let number = phonenumber::parse(own.country().id(), text.trim()).ok()?;
    phonenumber::is_valid(&number).then_some(number)
}

/// Finds who `number` belongs to on Signal (their ACI, or only their PNI if they don't let
/// themselves be found by number) and makes a chat with them, named by the number unless we
/// know them; returns that chat.
async fn find_number(manager: &mut Mgr, ctx: &Ctx, number: &str) -> Result<ConversationId, String> {
    let own = manager.registration_data().phone_number.clone();
    let parsed = parse_number(number, &own).ok_or(format!("“{number}” isn't a phone number."))?;
    let shown = display_number(&parsed);
    let found = manager
        .discover_contacts_by_phone_number([&parsed])
        .await
        .map_err(|e| format!("failed to look the number up: {e}"))?;
    let who = found
        .into_iter()
        .find_map(|(_, who)| who)
        .ok_or(format!("Nobody on Signal has the number {shown}."))?;
    let thread = Thread::Contact(who);
    let id = conversation_id(&thread);
    if !ctx.threads.borrow().contains_key(&id) {
        let title = match manager.thread_title(&thread).await {
            Ok(title) if !title.is_empty() && title != who.service_id_string() => title,
            _ => shown.clone(),
        };
        let mut conversation = register(&ctx.threads, thread, title, shown, 0);
        conversation.trust = trust_of(manager, &ctx.tables, who).await;
        conversation.settings = ctx
            .tables
            .settings(&id)
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
        let _ = ctx.events.send(Event::Conversations(vec![conversation]));
    }
    Ok(id)
}

/// The safety number with `who`, as Signal shows it: 60 digits in groups of five.
async fn safety_number(manager: &Mgr, who: ServiceId) -> Option<String> {
    use presage::libsignal_service::protocol::{Fingerprint, IdentityKeyStore};
    use presage::store::Store as _;
    let store = manager.store().aci_protocol_store();
    let ours = store.get_identity_key_pair().await.ok()?;
    let theirs = key_shown(manager, who).await?;
    let own = own_id(manager);
    // Version 2 identifies both sides by their ACI.
    let fingerprint = Fingerprint::new(
        2,
        5200,
        own.raw_uuid().as_bytes(),
        ours.identity_key(),
        who.raw_uuid().as_bytes(),
        &theirs,
    )
    .ok()?;
    let digits = fingerprint.display_string().ok()?;
    let groups: Vec<&str> = digits
        .as_bytes()
        .chunks(5)
        .filter_map(|chunk| std::str::from_utf8(chunk).ok())
        .collect();
    Some(groups.join(" "))
}

/// Messages of a thread with attachments (other than a long message's text) or links;
/// newest first, at most [`MEDIA_MESSAGES`].
async fn media(
    manager: &Mgr,
    thread: &Thread,
    conversation: &ConversationId,
) -> Result<Vec<ChatMessage>, String> {
    const MEDIA_MESSAGES: usize = 2000;
    const BATCH: u32 = 500;
    let mut names = Names::new();
    let mut found = Vec::new();
    let mut before = None;
    loop {
        let batch = manager
            .store()
            .messages_before(thread, before, BATCH)
            .await
            .map_err(|e| format!("failed to gather media: {e}"))?;
        for content in &batch {
            let wanted = data_of(content).is_some_and(|(data, _)| {
                data.attachments
                    .iter()
                    .any(|a| a.content_type.as_deref() != Some(attachments::LONG_TEXT))
                    || data
                        .body
                        .as_deref()
                        .is_some_and(|body| body.contains("http://") || body.contains("https://"))
            });
            if wanted
                && let Some(message) =
                    to_chat_message(manager, &mut names, conversation.clone(), content).await
            {
                found.push(message);
                if found.len() == MEDIA_MESSAGES {
                    return Ok(found);
                }
            }
        }
        if batch.len() < BATCH as usize {
            return Ok(found);
        }
        before = batch.last().map(stored_at);
    }
}

/// Messages of a thread whose text contains `query`, ignoring case; newest first, at most
/// [`SEARCH_HITS`].
async fn search(
    manager: &Mgr,
    thread: &Thread,
    conversation: &ConversationId,
    query: &str,
) -> Result<Vec<ChatMessage>, String> {
    const SEARCH_HITS: usize = 100;
    const BATCH: u32 = 500;
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut names = Names::new();
    let mut hits = Vec::new();
    let mut before = None;
    loop {
        let batch = manager
            .store()
            .messages_before(thread, before, BATCH)
            .await
            .map_err(|e| format!("failed to search: {e}"))?;
        for content in &batch {
            // Cheap test first: most messages don't match.
            let matches = data_of(content)
                .and_then(|(data, _)| data.body.as_deref())
                .is_some_and(|body| body.to_lowercase().contains(&query));
            if matches
                && let Some(message) =
                    to_chat_message(manager, &mut names, conversation.clone(), content).await
            {
                hits.push(message);
                if hits.len() == SEARCH_HITS {
                    return Ok(hits);
                }
            }
        }
        if batch.len() < BATCH as usize {
            return Ok(hits);
        }
        before = batch.last().map(stored_at);
    }
}

/// A new message of ours, ready to go (attachments uploaded).
struct Outgoing {
    body: String,
    styles: Vec<StyleRange>,
    mentions: Vec<crate::Mention>,
    quote: Option<Quote>,
    pointers: Vec<presage::proto::AttachmentPointer>,
    /// The preview of a link in it, as sent and as shown.
    preview: Option<(presage::proto::Preview, crate::LinkPreview)>,
    /// It is this sticker.
    sticker: Option<presage::proto::data_message::Sticker>,
}

/// Sends a new message and shows it; remembers it for receipts and starts its timer. Fails
/// only if it didn't go out: what is left to do after that is logged, not returned, so a sent
/// message always shows.
async fn send_new(
    manager: &mut Mgr,
    ctx: &Ctx,
    conversation: ConversationId,
    outgoing: Outgoing,
    timestamp: u64,
) -> Result<(), String> {
    let Outgoing {
        body,
        styles,
        mentions,
        quote,
        pointers,
        preview,
        sticker,
    } = outgoing;
    let (sent_preview, shown_preview) = preview.unzip();
    let thread = ctx
        .threads
        .borrow()
        .get(&conversation)
        .cloned()
        .ok_or("unknown conversation")?;
    let (text, ranges) = format::encode(&body, &styles, &mentions);
    // presage fills in the data message's timestamp from the one passed here.
    let message = DataMessage {
        body: (!text.is_empty()).then_some(text),
        body_ranges: body_ranges(ranges),
        attachments: pointers.clone(),
        preview: sent_preview.into_iter().collect(),
        sticker,
        quote: quote
            .as_ref()
            .map(|quote| presage::proto::data_message::Quote {
                id: Some(quote.timestamp),
                author_aci: Some(quote.author_id.clone()),
                author_aci_binary: aci_binary(&quote.author_id),
                text: Some(quote.text.clone()),
                ..Default::default()
            }),
        ..Default::default()
    };
    // As receivers will see them, stickers included.
    let shown_attachments = attachments::of_message(&message);
    let sent = send_data(manager, ctx, &thread, message, timestamp).await;
    // Sending may have met their new safety number, in the keys fetched to write to them.
    if let Thread::Contact(recipient) = thread {
        announce_identity_change(manager, ctx, &mut Names::new(), recipient).await;
    }
    sent?;
    let own = own_id(manager).service_id_string();
    // presage sent it with the thread's timer.
    let timer = expire_timer(manager, &thread).await;
    let reference = MessageRef {
        author_id: own.clone(),
        timestamp,
    };
    let _ = ctx.events.send(Event::Message(ChatMessage {
        conversation: conversation.clone(),
        sender_id: own,
        sender: "You".into(),
        body,
        timestamp,
        outgoing: true,
        quote,
        attachments: shown_attachments,
        styles,
        expire_timer: (timer > 0).then_some(timer),
        preview: shown_preview,
        ..Default::default()
    }));
    if let Err(error) = ctx
        .tables
        .set_delivery(&conversation, timestamp, Delivery::Sent)
        .await
    {
        warn!(%error, "failed to save that a message was sent");
    }
    // Writing to someone accepts the chat.
    let accepted = ctx
        .tables
        .settings(&conversation)
        .await
        .ok()
        .flatten()
        .is_some_and(|s| s.accepted);
    if !accepted
        && let Ok(settings) = ctx
            .tables
            .update(&conversation, |s| s.accepted = true)
            .await
    {
        let _ = ctx.events.send(Event::Settings {
            conversation: conversation.clone(),
            settings,
        });
    }
    start_timers(ctx, &conversation, &[(reference, timer)], timestamp).await;
    Ok(())
}

/// Edge of the pictures in the sticker picker: 64 px at up to 2x scaling.
const STICKER_THUMBNAIL: u32 = 128;

/// The installed sticker packs with small pictures of their stickers.
async fn sticker_packs(manager: &Mgr) -> Vec<crate::StickerPack> {
    let Ok(packs) = manager.store().sticker_packs().await else {
        return Vec::new();
    };
    packs
        .flatten()
        .map(|pack| crate::StickerPack {
            id: hex::encode(&pack.id),
            title: pack.manifest.title.clone(),
            stickers: pack
                .manifest
                .stickers
                .iter()
                .filter_map(|sticker| {
                    let picture =
                        attachments::thumbnail(sticker.bytes.as_deref()?, STICKER_THUMBNAIL)?;
                    Some(crate::StickerItem {
                        id: sticker.id,
                        emoji: sticker.emoji.clone().unwrap_or_default(),
                        picture: Arc::new(picture),
                    })
                })
                .collect(),
        })
        .filter(|pack| !pack.stickers.is_empty())
        .collect()
}

/// A sticker of an installed pack, ready to send: its picture uploaded.
async fn sticker_message(
    manager: &Mgr,
    pack: &str,
    sticker: u32,
) -> Result<presage::proto::data_message::Sticker, String> {
    let pack_id = hex::decode(pack).map_err(|_| "unknown sticker pack".to_string())?;
    let pack = manager
        .store()
        .sticker_pack(&pack_id)
        .await
        .ok()
        .flatten()
        .ok_or("unknown sticker pack")?;
    let found = pack
        .manifest
        .stickers
        .iter()
        .find(|s| s.id == sticker)
        .ok_or("unknown sticker")?;
    let bytes = found.bytes.clone().ok_or("the sticker isn't downloaded")?;
    let content_type = found
        .content_type
        .clone()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| "image/webp".into());
    let pointer = attachments::upload_bytes(manager, "sticker", &content_type, bytes).await?;
    Ok(presage::proto::data_message::Sticker {
        pack_id: Some(pack.id.clone()),
        pack_key: Some(pack.key.clone()),
        sticker_id: Some(sticker),
        data: Some(pointer),
        emoji: found.emoji.clone(),
    })
}

/// A preview of the first https link in `body`, its picture uploaded; `None` if there is no
/// link or the page says nothing about itself.
async fn link_preview(
    manager: &Mgr,
    body: &str,
) -> Option<(presage::proto::Preview, crate::LinkPreview)> {
    let url = format::links(body)
        .into_iter()
        .find(|link| link.starts_with("https://"))?
        .to_owned();
    let page = crate::preview::fetch_page(&url).await?;
    let mut pointer = None;
    if let Some(image) = &page.image
        && let Some((bytes, content_type)) = crate::preview::fetch_image(image).await
    {
        match attachments::upload_bytes(manager, "preview", &content_type, bytes).await {
            Ok(uploaded) => pointer = Some(uploaded),
            Err(error) => debug!(%error, "couldn't upload a link preview's picture"),
        }
    }
    let shown = crate::LinkPreview {
        url: url.clone(),
        title: page.title.clone(),
        description: page.description.clone(),
        image: pointer
            .as_ref()
            .map(|pointer| attachments::describe(pointer, false)),
    };
    let sent = presage::proto::Preview {
        url: Some(url),
        title: Some(page.title),
        description: (!page.description.is_empty()).then_some(page.description),
        image: pointer,
        date: None,
    };
    Some((sent, shown))
}

/// Tells our other devices about something done here.
async fn send_sync(manager: &mut Mgr, sync: SyncMessage) {
    let own = own_id(manager);
    if let Err(error) = manager
        .send_message(own, ContentBody::SynchronizeMessage(sync), now_millis())
        .await
    {
        warn!(%error, "failed to tell our other devices");
    }
}

/// Tells our other devices whom we block now: the whole list, as Signal expects.
async fn send_blocked(mut manager: Mgr, ctx: &Ctx) {
    let Ok(all) = ctx.tables.conversations().await else {
        return;
    };
    let mut blocked = sync_message::Blocked::default();
    for id in all.iter().filter(|(_, s)| s.blocked).map(|(id, _)| id) {
        let Some(thread) = thread_of(id) else {
            continue;
        };
        let number = match thread {
            Thread::Contact(who @ ServiceId::Pni(_)) => e164_of(&manager, who).await,
            _ => None,
        };
        list_blocked(&mut blocked, &thread, number);
    }
    let sync = SyncMessage {
        content: Some(SyncContent::Blocked(blocked)),
        ..Default::default()
    };
    send_sync(&mut manager, sync).await;
}

/// Answers a message request, here and on our other devices.
async fn respond(
    manager: &mut Mgr,
    ctx: &Ctx,
    conversation: &ConversationId,
    thread: &Thread,
    response: crate::RequestResponse,
) -> Result<(), String> {
    use crate::RequestResponse;
    use sync_message::message_request_response::Type;
    let db = |error: sqlx::Error| format!("failed to save: {error}");
    let kind = match response {
        RequestResponse::Accept => {
            ctx.tables
                .update(conversation, |s| s.accepted = true)
                .await
                .map_err(db)?;
            // Accepting shares our profile (name, picture) with them: presage puts our profile
            // key into every data message.
            let message = DataMessage {
                flags: Some(Flags::ProfileKeyUpdate as u32),
                ..Default::default()
            };
            send_data(manager, ctx, thread, message, now_millis()).await?;
            Type::Accept
        }
        RequestResponse::Delete => {
            let mut store = manager.store().clone();
            store
                .clear_thread(thread)
                .await
                .map_err(|e| format!("failed to delete the chat: {e}"))?;
            let _ = ctx.events.send(Event::Cleared(conversation.clone()));
            Type::Delete
        }
        RequestResponse::Block => {
            ctx.tables
                .update(conversation, |s| s.blocked = true)
                .await
                .map_err(db)?;
            send_blocked(manager.clone(), ctx).await;
            Type::Block
        }
    };
    let mut answer = sync_message::MessageRequestResponse {
        r#type: Some(kind as i32),
        ..Default::default()
    };
    match thread {
        Thread::Contact(who) => {
            answer.thread_aci = Some(who.service_id_string());
            answer.thread_aci_binary = Some(who.service_id_binary());
        }
        Thread::Group(key) => answer.group_id = Some(group_identifier(key).to_vec()),
    }
    let sync = SyncMessage {
        content: Some(SyncContent::MessageRequestResponse(answer)),
        ..Default::default()
    };
    send_sync(manager, sync).await;
    Ok(())
}

/// Tells a conversation we started or stopped typing.
async fn send_typing(mut manager: Mgr, thread: Thread, typing: bool) {
    use typing_message::Action;
    let timestamp = now_millis();
    let mut message = TypingMessage {
        timestamp: Some(timestamp),
        action: Some(if typing {
            Action::Started
        } else {
            Action::Stopped
        } as i32),
        group_id: None,
    };
    if let Thread::Group(key) = &thread {
        message.group_id = Some(group_identifier(key).to_vec());
    }
    let body = ContentBody::TypingMessage(message);
    if let Err(error) = send_to(&mut manager, &thread, body, timestamp).await {
        debug!(
            error = String::from(error),
            "failed to send a typing indicator"
        );
    }
}

/// Tells the senders of `messages` we read them (with `receipts`), and our other devices.
async fn send_read(mut manager: Mgr, messages: Vec<MessageRef>, receipts: bool) {
    let timestamp = now_millis();
    let own = own_id(&manager);
    if receipts {
        let mut by_author: HashMap<&str, Vec<u64>> = HashMap::new();
        for message in &messages {
            by_author
                .entry(&message.author_id)
                .or_default()
                .push(message.timestamp);
        }
        for (author, timestamps) in by_author {
            let Some(author) = ServiceId::parse_from_service_id_string(author) else {
                continue;
            };
            if author == own {
                continue;
            }
            let receipt = ReceiptMessage {
                r#type: Some(receipt_message::Type::Read as i32),
                timestamp: timestamps,
            };
            if let Err(error) = manager
                .send_message(author, ContentBody::ReceiptMessage(receipt), timestamp)
                .await
            {
                debug!(%error, "failed to send a read receipt");
            }
        }
    }
    let read = messages
        .into_iter()
        .map(|message| sync_message::Read {
            sender_aci_binary: aci_binary(&message.author_id),
            sender_aci: Some(message.author_id),
            timestamp: Some(message.timestamp),
        })
        .collect();
    let sync = SyncMessage {
        read,
        ..Default::default()
    };
    if let Err(error) = manager
        .send_message(own, ContentBody::SynchronizeMessage(sync), timestamp)
        .await
    {
        debug!(%error, "failed to tell our other devices what was read");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sync message says what our account did elsewhere, so only our own devices may send
    /// one; a contact's is dropped before it can clear chats or unblock people.
    #[test]
    fn sync_messages_count_only_from_our_own_devices() {
        use presage::libsignal_service::{content::Metadata, push_service::DEFAULT_DEVICE_ID};
        let who = |n: u8| {
            ServiceId::parse_from_service_id_string(&format!(
                "00000000-0000-0000-0000-0000000000{n:02}"
            ))
            .unwrap()
        };
        let (own, other) = (who(1), who(2));
        let content = |sender: ServiceId, body: ContentBody| Content {
            metadata: Metadata {
                sender,
                destination: own,
                sender_device: *DEFAULT_DEVICE_ID,
                pni_verified: None,
                client_timestamp: chrono::DateTime::from_timestamp_millis(1).unwrap(),
                server_timestamp: chrono::DateTime::from_timestamp_millis(1).unwrap(),
                needs_receipt: false,
                unidentified_sender: false,
                was_plaintext: false,
                server_guid: None,
            },
            body,
        };
        let sync = || {
            ContentBody::SynchronizeMessage(SyncMessage {
                content: Some(SyncContent::Blocked(sync_message::Blocked::default())),
                ..Default::default()
            })
        };
        let data = || {
            ContentBody::DataMessage(DataMessage {
                body: Some("hi".into()),
                ..Default::default()
            })
        };
        assert!(from_our_account(own, &content(own, sync())));
        assert!(!from_our_account(own, &content(other, sync())));
        assert!(from_our_account(own, &content(other, data())));
        assert!(from_our_account(own, &content(own, data())));
    }

    /// What the store saves when a contact's safety number changes is recognised as that note
    /// (from the store itself, so a change in how presage writes it shows up here); a real
    /// `Verified` sync message from our devices, which names its contact, is not.
    #[test]
    fn a_changed_safety_number_is_recognised() {
        use presage::proto::verified::State;
        use presage::{
            libsignal_service::protocol::{
                DeviceId, IdentityKeyPair, IdentityKeyStore, ProtocolAddress,
            },
            store::Store as _,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let store = SqliteStore::open(":memory:", OnNewIdentity::Trust)
                .await
                .unwrap();
            let contact =
                ServiceId::parse_from_service_id_string("00000000-0000-0000-0000-000000000002")
                    .unwrap();
            let address =
                ProtocolAddress::new(contact.service_id_string(), DeviceId::new(1).unwrap());
            let mut identities = store.aci_protocol_store();
            // Their first key, then a new one (e.g. they reinstalled Signal).
            for _ in 0..2 {
                let key = *IdentityKeyPair::generate(&mut rand::rng()).identity_key();
                identities.save_identity(&address, &key).await.unwrap();
            }
            let stored: Vec<Content> = store
                .messages(&Thread::Contact(contact), ..)
                .await
                .unwrap()
                .flatten()
                .collect();
            assert_eq!(stored.len(), 1, "one note, for the one change");
            assert_eq!(identity_note(&stored[0]), Some(State::Default));

            let mut real = stored[0].clone();
            let ContentBody::SynchronizeMessage(SyncMessage {
                content: Some(SyncContent::Verified(verified)),
                ..
            }) = &mut real.body
            else {
                panic!("the note is a Verified sync message");
            };
            let mut verifying = stored[0].clone();
            verified.destination_aci = Some(contact.service_id_string());
            assert_eq!(identity_note(&real), None);
            // Nor is a note of verifying the key.
            if let ContentBody::SynchronizeMessage(SyncMessage {
                content: Some(SyncContent::Verified(verified)),
                ..
            }) = &mut verifying.body
            {
                verified.set_state(presage::proto::verified::State::Verified);
            }
            assert_eq!(
                identity_note(&verifying),
                Some(State::Verified),
                "a note of verifying, not of a change"
            );
        });
    }

    /// A note in the chat for verifying, and for clearing a verification; none for repeating
    /// what was already so, nor for clearing what wasn't verified (e.g. their key changed since).
    #[test]
    fn verifications_get_a_note_only_when_it_says_something() {
        use crate::local::Verification;
        use presage::proto::verified::State::{self, Default as Cleared, Unverified, Verified};
        let of = |key: u8, state: State| Verification {
            identity_key: vec![key; 33],
            state,
        };
        assert_eq!(verification_note(None, &of(1, Verified)), Some(Verified));
        assert_eq!(
            verification_note(Some(&of(1, Verified)), &of(1, Verified)),
            None
        );
        assert_eq!(
            verification_note(Some(&of(1, Verified)), &of(1, Cleared)),
            Some(Unverified),
            "cleared"
        );
        assert_eq!(verification_note(None, &of(1, Cleared)), None);
        assert_eq!(
            verification_note(Some(&of(1, Cleared)), &of(1, Cleared)),
            None
        );
        // Verified their old key; clearing for the new one says nothing of it.
        assert_eq!(
            verification_note(Some(&of(1, Verified)), &of(2, Cleared)),
            None
        );
        assert_eq!(
            verification_note(Some(&of(1, Verified)), &of(2, Verified)),
            Some(Verified)
        );
        // Another device saw their key change: the change notice says so already.
        assert_eq!(
            verification_note(Some(&of(1, Verified)), &of(2, Unverified)),
            None
        );
    }

    /// What tells our other devices about a verification is what Signal's apps send: the
    /// contact's ACI both ways, their serialized key, the state (cleared to the default), and the
    /// null message's padding; and what they send back is read the same way.
    #[test]
    fn verifications_to_and_from_other_devices() {
        use presage::{libsignal_service::protocol::IdentityKeyPair, proto::verified::State};
        let who = ServiceId::parse_from_service_id_string("00000000-0000-0000-0000-000000000002")
            .unwrap();
        let key = *IdentityKeyPair::generate(&mut rand::rng()).identity_key();
        let padding = Some(vec![7; 12]);
        let verified_of = |sync: SyncMessage| match sync.content {
            Some(SyncContent::Verified(verified)) => verified,
            other => panic!("not a verification: {other:?}"),
        };

        let marked = verified_of(verified_sync(who, &key, true, padding.clone()));
        assert_eq!(marked.destination_aci, Some(who.service_id_string()));
        assert_eq!(marked.destination_aci_binary, Some(who.service_id_binary()));
        assert_eq!(marked.identity_key.as_deref().map(<[u8]>::len), Some(33));
        assert_eq!(marked.state(), State::Verified);
        assert_eq!(marked.null_message, padding);
        let cleared = verified_of(verified_sync(who, &key, false, None));
        assert_eq!(cleared.state(), State::Default);

        assert_eq!(verified_target(&marked), Some((who, key)));
        let by_string = presage::proto::Verified {
            destination_aci_binary: None,
            ..marked.clone()
        };
        assert_eq!(verified_target(&by_string), Some((who, key)));
        let pni = presage::proto::Verified {
            destination_aci: None,
            destination_aci_binary: Some(
                ServiceId::parse_from_service_id_string("PNI:00000000-0000-0000-0000-000000000002")
                    .unwrap()
                    .service_id_binary(),
            ),
            ..marked.clone()
        };
        assert_eq!(verified_target(&pni), None, "only contacts' ACIs");
        let keyless = presage::proto::Verified {
            identity_key: None,
            ..marked
        };
        assert_eq!(verified_target(&keyless), None);
    }

    /// Verified only for the key it was verified for; a new key of a verified contact is a
    /// change to confirm; clearing (for any key) leaves nothing to confirm.
    #[test]
    fn trust_follows_the_key() {
        use crate::{Trust, local::Verification};
        use presage::{
            libsignal_service::protocol::{IdentityKey, IdentityKeyPair},
            proto::verified::State::{self, Default as Cleared, Unverified, Verified},
        };
        let old = *IdentityKeyPair::generate(&mut rand::rng()).identity_key();
        let new = *IdentityKeyPair::generate(&mut rand::rng()).identity_key();
        let of = |key: &IdentityKey, state: State| Verification {
            identity_key: key.serialize().to_vec(),
            state,
        };
        assert_eq!(trust(None, Some(&new)), Trust::Unverified);
        assert_eq!(
            trust(Some(&of(&new, Verified)), Some(&new)),
            Trust::Verified
        );
        assert_eq!(trust(Some(&of(&old, Verified)), Some(&new)), Trust::Changed);
        assert_eq!(
            trust(Some(&of(&old, Cleared)), Some(&new)),
            Trust::Unverified
        );
        assert_eq!(
            trust(Some(&of(&new, Cleared)), Some(&new)),
            Trust::Unverified,
            "confirmed"
        );
        // The phone saw it change after it was verified, and it isn't confirmed yet.
        assert_eq!(
            trust(Some(&of(&new, Unverified)), Some(&new)),
            Trust::Changed
        );
        assert_eq!(
            trust(Some(&of(&old, Verified)), None),
            Trust::Unverified,
            "no key yet"
        );
    }

    /// A send the store refused for a contact's identity is recognised in each way it comes
    /// back, and names that contact; other failures are just failures.
    #[test]
    fn a_refused_identity_names_the_contact() {
        use presage::libsignal_service::{
            prelude::MessageSenderError,
            protocol::{DeviceId, ProtocolAddress, SignalProtocolError},
        };
        let who = ServiceId::parse_from_service_id_string("00000000-0000-0000-0000-000000000002")
            .unwrap();
        let address = ProtocolAddress::new(who.service_id_string(), DeviceId::new(1).unwrap());
        let refused = || SignalProtocolError::UntrustedIdentity(address.clone());
        let errors: [PresageError<presage_store_sqlite::SqliteStoreError>; 3] = [
            MessageSenderError::UntrustedIdentity { address: who }.into(),
            MessageSenderError::ProtocolError(refused()).into(),
            PresageError::ProtocolError(refused()),
        ];
        for error in &errors {
            assert_eq!(untrusted(error), Some(who), "{error}");
        }
        let other: PresageError<presage_store_sqlite::SqliteStoreError> =
            MessageSenderError::MaximumRetriesLimitExceeded.into();
        assert_eq!(untrusted(&other), None);
    }

    /// A number typed internationally, or the way our own country writes them, is the same
    /// number; text that isn't one, or an invalid number, is none.
    /// Current Signal apps fill only the binary ACI fields, and read only those when given
    /// both; b33p reads either and fills both.
    #[test]
    fn acis_are_read_and_written_in_binary_too() {
        use presage::proto::data_message::Reaction;
        let id = "00000000-0000-0000-0000-000000000007";
        let aci = ServiceId::parse_from_service_id_string(id).unwrap();
        let binary = aci_binary(id).unwrap();
        assert_eq!(binary, aci.service_id_binary());
        assert_eq!(binary.len(), 16);
        assert_eq!(aci_binary(&format!("PNI:{id}")), None);
        assert_eq!(aci_binary("not an id"), None);

        let target = MessageRef {
            author_id: id.into(),
            timestamp: 5,
        };
        let from_phone = Reaction {
            target_author_aci_binary: Some(binary.clone()),
            target_sent_timestamp: Some(5),
            ..Default::default()
        };
        assert_eq!(reaction_target(&from_phone), Some(target.clone()));
        let from_older_client = Reaction {
            target_author_aci: Some(id.into()),
            target_sent_timestamp: Some(5),
            ..Default::default()
        };
        assert_eq!(reaction_target(&from_older_client), Some(target));
        let nameless = Reaction {
            target_sent_timestamp: Some(5),
            ..Default::default()
        };
        assert_eq!(reaction_target(&nameless), None);
    }

    /// The phone takes the list as the whole one and unblocks everyone left out of it, so
    /// everyone must be in it in a form it reads.
    #[test]
    fn the_blocked_list_names_everyone_as_the_phone_reads_them() {
        let aci = ServiceId::parse_from_service_id_string("00000000-0000-0000-0000-000000000007")
            .unwrap();
        let pni =
            ServiceId::parse_from_service_id_string("PNI:00000000-0000-0000-0000-000000000008")
                .unwrap();
        let mut blocked = sync_message::Blocked::default();
        list_blocked(&mut blocked, &Thread::Contact(aci), None);
        list_blocked(
            &mut blocked,
            &Thread::Contact(pni),
            Some("+4915123456789".into()),
        );
        // Without a known number there is nothing to name them by.
        list_blocked(&mut blocked, &Thread::Contact(pni), None);
        assert_eq!(blocked.acis, [aci.service_id_string()]);
        assert_eq!(blocked.acis_binary, [aci.service_id_binary()]);
        assert_eq!(blocked.numbers, ["+4915123456789"]);
        assert!(blocked.group_ids.is_empty());
    }

    #[test]
    fn numbers_are_read_the_way_our_country_writes_them() {
        use presage::libsignal_service::prelude::phonenumber;
        let own = phonenumber::parse(None, "+49 151 23456789").unwrap();
        // As contact discovery gets them.
        let e164 = |text| {
            parse_number(text, &own).map(|n| n.format().mode(phonenumber::Mode::E164).to_string())
        };
        assert_eq!(e164("+49 1512 3456780").as_deref(), Some("+4915123456780"));
        assert_eq!(e164(" 01512 3456780 ").as_deref(), Some("+4915123456780"));
        assert!(
            parse_number("+44 7911 123456", &own).is_some(),
            "another country's"
        );
        assert_eq!(parse_number("Ann", &own), None);
        assert_eq!(parse_number("+49 1", &own), None, "too short to be valid");
    }

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
        let dir = std::env::temp_dir().join(format!("b33p-db-{}", std::process::id()));
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
