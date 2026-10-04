//! Real backend: presage on a single-threaded tokio runtime (presage's futures are `!Send`).

use std::{cell::RefCell, collections::HashMap, io, path::PathBuf, rc::Rc, sync::{Arc, mpsc}, time::Duration};

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
    store::{ContentExt, ContentsStore, Thread},
};
use presage_store_sqlite::SqliteStore;
use sqlx::{
    ConnectOptions,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous},
};
use tokio::{runtime::Runtime, task::LocalSet};
use tracing::{debug, info, warn};

use crate::{
    Avatar, transfer,
    ChatMessage, Command, Config, Conversation, ConversationId, Event, Events, Inbox, Password, QrMatrix,
    now_millis,
    vault::{self, KeySource, KeyringKey},
};

pub(crate) type Mgr = Manager<SqliteStore, Registered>;

/// Maps the opaque ids handed to the UI back to presage threads.
type Threads = Rc<RefCell<HashMap<ConversationId, Thread>>>;

pub fn run(config: Config, commands: Inbox, events: Events) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    let local = tokio::task::LocalSet::new();
    let (result, closed) = match unlock(&runtime, &local, &commands, &events) {
        Ok(Some((store, closed))) => {
            (local.block_on(&runtime, session(config, store, commands, events.clone())), Some(closed))
        }
        // The UI went away while we were waiting for a password.
        Ok(None) => (Ok(()), None),
        Err(error) => (Err(error), None),
    };
    if let Err(error) = result {
        let _ = events.send(Event::Error(error));
    }
    // Drop the tasks still holding the store, then wait for its connections to close, so
    // `Backend::shutdown` returns only once the database is no longer in use.
    drop(local);
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
        let (dir, keyring_account) = match std::env::var_os("LANGIS_DATA_DIR") {
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
        // 0700: the database inside is only as private as the directory holding it.
        vault::restrict_dir(&dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
        Ok(Self { db: dir.join("langis.db3"), key_source: dir.join("langis.key"), dir, keyring_account })
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
    let salt = match KeySource::read(&paths.key_source).map_err(|e| e.to_string())? {
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
        Some(KeySource::Password { salt }) => salt,
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
                write_source(&paths, &KeySource::Password { salt })?;
                salt
            }
        },
    };

    // Until the database exists, the password is being chosen rather than checked.
    let new = !paths.db.exists();
    let mut error = None;
    loop {
        let _ = events.send(Event::PasswordRequired { new, error: error.take() });
        let Some(password) = wait_for_password(commands) else { return Ok(None) };
        let key = vault::password_key(password.expose(), &salt);
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
        // The receive task and the command loop both use the pool, so a write can briefly
        // overlap a read; without these, SQLite answers SQLITE_BUSY instead of waiting. WAL
        // also keeps long readers from blocking the sending task.
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
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
        // `Reject`, not `Trust`: presage trusts an identity it has never seen before (that is ordinary
        // trust-on-first-use), but `Trust` would also silently accept a *changed* key for a
        // known contact, which is exactly the impersonation case safety numbers exist to catch.
        // A legitimate re-key — new phone, reinstall — then stops delivering until the user
        // re-links, which is the safe direction to fail.
        .block_on(runtime, SqliteStore::open_with_options(options, OnNewIdentity::Reject))
        .map_err(|e| format!("failed to open the database: {e}"))?;
    // SQLite creates the database (and its write-ahead log and shared memory files) with the
    // process umask, which is typically world-readable. On a shared machine that would let any
    // other user copy the database and attack the password offline.
    let files = [paths.db.clone(), paths.db.with_extension("db3-wal"), paths.db.with_extension("db3-shm")];
    for path in files {
        if let Err(error) = vault::restrict_file(&path)
            && error.kind() != io::ErrorKind::NotFound
        {
            warn!(path = %path.display(), %error, "could not restrict the database's permissions");
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

    let (mut manager, history_key) = match Manager::load_registered(store.clone()).await {
        Ok(manager) => (manager, None),
        Err(PresageError::NotYetRegisteredError) => {
            // Nothing can be done before linking, so commands are dropped until then.
            let ui_closed = async { while command_rx.recv().await.is_some() {} };
            tokio::select! {
                linked = link(store, &config, &events) => linked?,
                () = ui_closed => return Ok(()),
            }
        }
        Err(error) => return Err(format!("failed to load registration: {error}")),
    };

    let account = manager
        .registration_data()
        .service_ids
        .aci()
        .service_id_string();
    let _ = events.send(Event::Linked { account });

    // The history has to be in place before live messages arrive, so this comes first.
    // Commands that arrive meanwhile are handled afterwards.
    let mut pending = Vec::new();
    if let Some(key) = history_key {
        let dir = Paths::new()?.dir;
        let transfer = transfer::receive_history(&manager, key, &dir);
        pin_mut!(transfer);
        let result = loop {
            tokio::select! {
                result = &mut transfer => break result,
                command = command_rx.recv() => match command {
                    Some(command) => pending.push(command),
                    None => return Ok(()),
                },
            }
        };
        if let Err(error) = result {
            warn!(%error, "message history transfer failed");
            let _ = events.send(Event::Error(error));
        }
    }

    let threads: Threads = Rc::default();
    if send_conversations(&manager, &threads, &events).await == 0 {
        // Phones don't push their contacts to a newly linked device; they have to be asked.
        // The answer arrives through `receive` as `Received::Contacts`.
        info!("no contacts yet; requesting them from the primary device");
        if let Err(error) = manager.request_contacts().await {
            warn!(%error, "failed to request contacts");
        }
    }

    send_latest_messages(&manager, &threads, &events).await;

    tokio::task::spawn_local(receive(manager.clone(), threads.clone(), events.clone()));
    tokio::task::spawn_local(fetch_avatars(manager.clone(), threads.clone(), events.clone()));

    for command in pending {
        if let Err(error) = handle(&mut manager, &threads, &events, command).await {
            let _ = events.send(Event::Error(error));
        }
    }
    while let Some(command) = command_rx.recv().await {
        if let Err(error) = handle(&mut manager, &threads, &events, command).await {
            let _ = events.send(Event::Error(error));
        }
    }
    Ok(())
}

/// Links this device; also returns the one-time key for the history transfer if the user
/// chose to transfer it.
async fn link(store: SqliteStore, config: &Config, events: &Events) -> Result<(Mgr, Option<[u8; 32]>), String> {
    let (url_tx, url_rx) = oneshot::channel();
    let (linked, _) = future::join(
        Manager::link_secondary_device_with_history(
            store,
            SignalServers::Production,
            config.device_name.clone(),
            url_tx,
        ),
        async {
            if let Ok(mut url) = url_rx.await
                && let Some(qr) = QrMatrix::encode(
                    url.query_pairs_mut().append_pair("capabilities", transfer::LINK_CAPABILITY).finish().as_str(),
                )
            {
                let _ = events.send(Event::LinkQr(qr));
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
fn register(threads: &Threads, thread: Thread, title: String) -> Conversation {
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
            let title = match (contact.name.is_empty(), &contact.phone_number) {
                (true, Some(number)) => number.to_string(),
                _ => contact.name,
            };
            all.push(register(threads, thread, title));
        }
    }
    let contacts = all.len();
    if let Ok(groups) = store.groups().await {
        for (key, group) in groups.flatten() {
            all.push(register(threads, Thread::Group(key), group.title));
        }
    }
    let _ = events.send(Event::Conversations(all));
    contacts
}

/// Sends each conversation's newest stored message, so the UI can order conversations by
/// activity and show previews before any history is opened.
async fn send_latest_messages(manager: &Mgr, threads: &Threads, events: &Events) {
    let threads: Vec<_> = threads.borrow().iter().map(|(id, thread)| (id.clone(), thread.clone())).collect();
    let mut names = Names::new();
    for (id, thread) in threads {
        let Ok(contents) = manager.store().messages(&thread, ..).await else { continue };
        // Newest first; skip receipts and the like.
        for content in contents.flatten() {
            if let Some(message) = to_chat_message(manager, &mut names, id.clone(), &content).await {
                let _ = events.send(Event::History { conversation: id, messages: vec![message] });
                break;
            }
        }
    }
}

/// Side of decoded avatars: a 40 px circle at up to 3x scaling.
const AVATAR_SIDE: u32 = 128;

/// Sends each conversation's profile or group picture. presage caches them in the store, so
/// only the first run downloads them, one at a time to stay clear of rate limits.
///
/// A profile picture needs the contact's profile key, which only arrives with their messages
/// (the contact sync no longer carries it). Until then the picture from the contact sync, i.e.
/// what the phone shows, stands in.
async fn fetch_avatars(mut manager: Mgr, threads: Threads, events: Events) {
    let threads: Vec<_> = threads.borrow().iter().map(|(id, thread)| (id.clone(), thread.clone())).collect();
    let (mut found, mut none, mut failed) = (0, 0, 0);
    for (id, thread) in threads {
        let picture = match thread {
            Thread::Contact(ServiceId::Aci(aci)) => {
                let id = ServiceId::Aci(aci);
                let profile = match manager.store().profile_key(&id).await {
                    Ok(Some(key)) => manager.retrieve_profile_avatar_by_uuid(aci.into(), key).await,
                    _ => Ok(None),
                };
                match profile {
                    Ok(None) => {
                        let contact = manager.store().contact_by_id(&id).await.ok().flatten();
                        Ok(contact.and_then(|c| c.avatar).map(|avatar| avatar.reader.to_vec()))
                    }
                    profile => profile,
                }
            }
            Thread::Contact(_) => continue,
            Thread::Group(key) => {
                // presage only refetches the group if this revision is newer than the cached one.
                let revision = manager.store().group(key).await.ok().flatten().map_or(0, |group| group.revision);
                let context =
                    GroupContextV2 { master_key: Some(key.to_vec()), revision: Some(revision), ..Default::default() };
                manager.retrieve_group_avatar(context).await
            }
        };
        match picture {
            Ok(Some(bytes)) => match decode_avatar(&bytes) {
                Some(avatar) => {
                    found += 1;
                    if !events.send(Event::Avatar { conversation: id, avatar: Arc::new(avatar) }) {
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
fn decode_avatar(bytes: &[u8]) -> Option<Avatar> {
    let image = image::load_from_memory(bytes).ok()?;
    let square = image.resize_to_fill(AVATAR_SIDE, AVATAR_SIDE, image::imageops::FilterType::Lanczos3);
    Some(Avatar { side: AVATAR_SIDE, rgba: square.into_rgba8().into_raw() })
}

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
async fn receive(mut manager: Mgr, threads: Threads, events: Events) {
    const MAX_BACKOFF: Duration = Duration::from_secs(60);
    let mut names = Names::new();
    let mut backoff = Duration::from_secs(1);
    loop {
        match manager.receive_messages().await {
            Ok(messages) => {
                pin_mut!(messages);
                while let Some(received) = messages.next().await {
                    // Something came through, so the connection works again.
                    backoff = Duration::from_secs(1);
                    if !handle_received(&manager, &threads, &events, &mut names, received).await {
                        return;
                    }
                }
                info!("connection to Signal closed");
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
    names: &mut Names,
    received: Received,
) -> bool {
    match received {
        Received::QueueEmpty => events.send(Event::Synced),
        Received::Contacts => {
            names.clear();
            send_conversations(manager, threads, events).await;
            // A newly linked device first learns its contacts here.
            tokio::task::spawn_local(fetch_avatars(manager.clone(), threads.clone(), events.clone()));
            true
        }
        Received::DecryptionError(sender) => {
            // With `OnNewIdentity::Reject` this is also what an identity *change* looks like: a
            // message from a contact whose identity key changed fails to decrypt and arrives
            // here. The envelope doesn't say which, so the user is told both are possible and
            // given the way to check.
            warn!(sender = %sender.service_id_string(), "decryption error (possibly an identity change)");
            let conversation = ensure_conversation(manager, threads, events, Thread::Contact(sender)).await;
            let name = sender_name(manager, names, sender).await;
            events.send(Event::Message(ChatMessage {
                conversation,
                sender_id: sender.service_id_string(),
                body: format!(
                    "⚠ A message from {name} couldn't be decrypted. It may be corrupt, or {name}'s \
                     safety number may have changed — compare it in Signal on your phone."
                ),
                sender: name,
                timestamp: now_millis(),
                outgoing: false,
            }))
        }
        Received::Content(content) => {
            let Ok(thread) = Thread::try_from(&*content) else { return true };
            let id = ensure_conversation(manager, threads, events, thread).await;
            match to_chat_message(manager, names, id, &content).await {
                Some(message) => events.send(Event::Message(message)),
                None => true,
            }
        }
    }
}

/// Registers `thread` and announces it to the UI if it's the first we hear of it.
async fn ensure_conversation(manager: &Mgr, threads: &Threads, events: &Events, thread: Thread) -> ConversationId {
    let id = conversation_id(&thread);
    if !threads.borrow().contains_key(&id) {
        let title = manager.thread_title(&thread).await.unwrap_or_default();
        let conversation = register(threads, thread, title);
        info!(title = conversation.title, "new conversation");
        events.send(Event::Conversations(vec![conversation]));
    }
    id
}

/// Extracts displayable text; returns `None` for receipts, typing indicators, etc.
async fn to_chat_message(
    manager: &Mgr,
    names: &mut Names,
    conversation: ConversationId,
    content: &Content,
) -> Option<ChatMessage> {
    let (body, outgoing) = match &content.body {
        ContentBody::DataMessage(DataMessage {
            body: Some(body), ..
        }) => (body.clone(), false),
        ContentBody::SynchronizeMessage(SyncMessage {
            content:
                Some(SyncContent::Sent(Sent {
                    message:
                        Some(DataMessage {
                            body: Some(body), ..
                        }),
                    ..
                })),
            ..
        }) => (body.clone(), true),
        _ => return None,
    };
    let sender = if outgoing {
        "You".to_string()
    } else {
        sender_name(manager, names, content.metadata.sender).await
    };
    Some(ChatMessage {
        conversation,
        sender_id: content.metadata.sender.service_id_string(),
        sender,
        body,
        timestamp: content.timestamp(),
        outgoing,
    })
}

async fn handle(
    manager: &mut Mgr,
    threads: &Threads,
    events: &Events,
    command: Command,
) -> Result<(), String> {
    match command {
        // Already unlocked.
        Command::Unlock(_) => {}
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
            for content in contents.flatten() {
                if let Some(message) =
                    to_chat_message(manager, &mut names, id.clone(), &content).await
                {
                    messages.push(message);
                }
            }
            let _ = events.send(Event::History {
                conversation: id,
                messages,
            });
        }
        Command::Send { conversation, body } => {
            let Some(thread) = threads.borrow().get(&conversation).cloned() else {
                return Err("unknown conversation".into());
            };
            // presage fills in the data message's timestamp from the one passed here.
            let timestamp = now_millis();
            let mut message = DataMessage {
                body: Some(body.clone()),
                ..Default::default()
            };
            let sent = match &thread {
                Thread::Contact(recipient) => {
                    manager.send_message(*recipient, message, timestamp).await
                }
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
            sent.map_err(|e| format!("failed to send: {e}"))?;
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
            }));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(dir: &std::path::Path) -> Paths {
        Paths {
            db: dir.join("test.db3"),
            key_source: dir.join("test.key"),
            dir: dir.to_path_buf(),
            keyring_account: "unused".into(),
        }
    }

    fn runtime() -> (tokio::runtime::Runtime, LocalSet) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let local = LocalSet::new();
        (runtime, local)
    }

    #[test]
    fn database_opens_only_with_its_key() {
        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let (runtime, local) = runtime();

        let key = vault::password_key("correct horse", &[3; 16]);
        drop(open_store(&runtime, &local, &paths, &key).expect("creates the database"));
        let wrong = vault::password_key("wrong horse", &[3; 16]);
        assert!(open_store(&runtime, &local, &paths, &wrong).is_err());
        assert!(open_store(&runtime, &local, &paths, &key).is_ok());
        // Encrypted: no plaintext SQLite header.
        let bytes = std::fs::read(&paths.db).unwrap();
        assert!(!bytes.starts_with(b"SQLite format 3"));
    }

    /// Only the owner may read the database, or another user on a shared machine could copy
    /// it and attack the password offline.
    #[cfg(unix)]
    #[test]
    fn the_database_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let paths = paths(dir.path());
        let (runtime, local) = runtime();
        let key = vault::password_key("correct horse", &[3; 16]);
        drop(open_store(&runtime, &local, &paths, &key).expect("creates the database"));

        assert_eq!(std::fs::metadata(&paths.db).unwrap().permissions().mode() & 0o777, 0o600);
    }

    /// The directory is created 0700, so the database is private before it even exists.
    #[cfg(unix)]
    #[test]
    fn the_data_directory_is_private_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("langis");
        vault::restrict_dir(&dir).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    }

    /// An existing directory left world-readable by an older version gets tightened.
    #[cfg(unix)]
    #[test]
    fn restrict_dir_tightens_an_existing_directory() {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("langis");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        vault::restrict_dir(&dir).unwrap();
        assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    }

    /// A half-written key source would lock the user out of their own history, so the file is
    /// renamed into place instead of being written in situ.
    #[test]
    fn the_key_source_is_replaced_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("langis.key");
        KeySource::Password { salt: [1; 16] }.write(&path).unwrap();
        KeySource::Keyring.write(&path).unwrap();
        assert_eq!(KeySource::read(&path).unwrap(), Some(KeySource::Keyring));
        // No temporary file is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".langis-"))
            .collect();
        assert!(leftovers.is_empty(), "left temporary files behind: {leftovers:?}");
    }

    #[cfg(unix)]
    #[test]
    fn the_key_source_is_not_world_readable() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("langis.key");
        KeySource::Keyring.write(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
