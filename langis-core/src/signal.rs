//! Real backend: presage on a single-threaded tokio runtime (presage's futures are `!Send`).

use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc, sync::{Arc, mpsc}, time::Duration};

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
use sqlx::{ConnectOptions, sqlite::SqliteConnectOptions};
use tokio::{runtime::Runtime, task::LocalSet};
use tracing::{debug, info, warn};

use crate::{
    Picture, attachments, transfer,
    ChatMessage, Command, Config, Quote, Conversation, ConversationId, Event, Events, HistoryTransfer, Inbox, Password,
    QrMatrix,
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
        std::fs::create_dir_all(&dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
        Ok(Self { db: dir.join("langis.db3"), key_source: dir.join("langis.key"), dir, keyring_account })
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
        // sqlx keeps its own copy of this string; it can't be wiped.
        .pragma("key", vault::sqlcipher_pragma(key).to_string())
        // The key is part of the SQL text (pragmas take no parameters), so statements must
        // never reach the log, whatever RUST_LOG says.
        .disable_statement_logging()
        .collation("langis_close_guard", move |a, b| {
            let _ = &open;
            a.cmp(b)
        });
    local
        .block_on(runtime, SqliteStore::open_with_options(options, OnNewIdentity::Trust))
        .map(|store| (store, closed))
        .map_err(|e| format!("failed to open the database: {e}"))
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
    let linked = match Manager::load_registered(store.clone()).await {
        Ok(manager) if !paths.transfer_marker().exists() => Some((manager, Vec::new())),
        Ok(manager) => {
            drop(manager);
            let _ = events.send(Event::Error(
                "The message history transfer was interrupted. Link this device again to retry.".into(),
            ));
            reset(store.clone(), &paths).await?;
            link_with_history(store, &config, &events, &mut command_rx, &paths).await?
        }
        Err(PresageError::NotYetRegisteredError) => {
            link_with_history(store, &config, &events, &mut command_rx, &paths).await?
        }
        Err(error) => return Err(format!("failed to load registration: {error}")),
    };
    // `None`: the UI went away while linking.
    let Some((mut manager, pending)) = linked else { return Ok(()) };

    let account = manager.registration_data().service_ids.aci().service_id_string();
    let _ = events.send(Event::Linked { account });

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

    let downloads = attachments::download_limit();
    tokio::task::spawn_local(receive(manager.clone(), threads.clone(), events.clone()));
    tokio::task::spawn_local(fetch_avatars(manager.clone(), threads.clone(), events.clone()));

    for command in pending {
        if let Err(error) = handle(&mut manager, &threads, &events, &downloads, command).await {
            let _ = events.send(Event::Error(error));
        }
    }
    while let Some(command) = command_rx.recv().await {
        if let Err(error) = handle(&mut manager, &threads, &events, &downloads, command).await {
            let _ = events.send(Event::Error(error));
        }
    }
    Ok(())
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
        let Some(key) = key else { return Ok(Some((manager, Vec::new()))) };

        write_marker(paths)?;
        let account = manager.registration_data().service_ids.aci().service_id_string();
        let _ = events.send(Event::Linked { account });
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
                std::fs::remove_file(paths.transfer_marker())
                    .map_err(|e| format!("failed to remove {}: {e}", paths.transfer_marker().display()))?;
                return Ok(Some((manager, pending)));
            }
            Ok(transfer::Outcome::Relink) => info!("the phone asked to link again"),
            Err(error) => {
                warn!(%error, "message history transfer failed");
                let _ = events.send(Event::Error(format!("{error} Link this device again to retry.")));
            }
        }
        drop(manager);
        reset(store.clone(), paths).await?;
    }
}

fn write_marker(paths: &Paths) -> Result<(), String> {
    std::fs::write(paths.transfer_marker(), b"")
        .map_err(|e| format!("failed to write {}: {e}", paths.transfer_marker().display()))
}

/// Forgets the registration and everything stored with it, ready to link again.
async fn reset(mut store: SqliteStore, paths: &Paths) -> Result<(), String> {
    store.clear_registration().await.map_err(|e| format!("failed to reset the database: {e}"))?;
    store.clear_contents().await.map_err(|e| format!("failed to reset the database: {e}"))?;
    match std::fs::remove_file(paths.transfer_marker()) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("failed to remove {}: {error}", paths.transfer_marker().display()))
        }
        _ => Ok(()),
    }
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
    // Messages to yourself; listed once there are some.
    all.push(register(threads, Thread::Contact(own_id(manager)), "Note to Self".into()));
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
                let _ = events.send(Event::Latest(message));
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
fn decode_avatar(bytes: &[u8]) -> Option<Picture> {
    let image = image::load_from_memory(bytes).ok()?;
    let square = image.resize_to_fill(AVATAR_SIDE, AVATAR_SIDE, image::imageops::FilterType::Lanczos3);
    Some(Picture { width: AVATAR_SIDE, height: AVATAR_SIDE, rgba: square.into_rgba8().into_raw() })
}

fn own_id(manager: &Mgr) -> ServiceId {
    ServiceId::Aci(manager.registration_data().service_ids.aci())
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
            warn!(sender = %sender.service_id_string(), "decryption error");
            // The envelope doesn't say whether it was a group message, so it goes in the 1:1 chat.
            let conversation = ensure_conversation(manager, threads, events, Thread::Contact(sender)).await;
            let name = sender_name(manager, names, sender).await;
            events.send(Event::Message(ChatMessage {
                conversation,
                sender_id: sender.service_id_string(),
                body: format!("⚠ A message from {name} couldn't be decrypted."),
                sender: name,
                timestamp: now_millis(),
                ..Default::default()
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
    let (data, outgoing) = match &content.body {
        ContentBody::DataMessage(data) => (data, false),
        ContentBody::SynchronizeMessage(SyncMessage {
            content: Some(SyncContent::Sent(Sent { message: Some(data), .. })),
            ..
        }) => (data, true),
        _ => return None,
    };
    let body = data.body.clone().unwrap_or_default();
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
            let author = match quote.author_aci.as_deref().and_then(ServiceId::parse_from_service_id_string) {
                Some(author) if author == own_id(manager) => "You".to_string(),
                Some(author) => sender_name(manager, names, author).await,
                None => String::new(),
            };
            Some(Quote { author, text: quote.text.clone().unwrap_or_default() })
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
            tokio::task::spawn_local(attachments::fetch(servers, attachment, events.clone(), downloads.clone()));
        }
        Command::SaveAttachment(attachment) => {
            tokio::task::spawn_local(attachments::save(servers, attachment, events.clone(), downloads.clone()));
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
    fn database_opens_only_with_its_key() {
        let dir = std::env::temp_dir().join(format!("langis-db-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths {
            db: dir.join("test.db3"),
            key_source: dir.join("test.key"),
            dir: dir.clone(),
            keyring_account: "unused".into(),
        };
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let local = LocalSet::new();

        let key = vault::password_key("correct horse", &[3; 16]);
        drop(open_store(&runtime, &local, &paths, &key).expect("creates the database"));
        let wrong = vault::password_key("wrong horse", &[3; 16]);
        assert!(open_store(&runtime, &local, &paths, &wrong).is_err());
        assert!(open_store(&runtime, &local, &paths, &key).is_ok());
        // Encrypted: no plaintext SQLite header.
        let bytes = std::fs::read(&paths.db).unwrap();
        assert!(!bytes.starts_with(b"SQLite format 3"));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
