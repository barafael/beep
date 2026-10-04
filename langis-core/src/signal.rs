//! Real backend: presage on a single-threaded tokio runtime (presage's futures are `!Send`).

use std::{cell::RefCell, collections::HashMap, path::PathBuf, rc::Rc};

use crossbeam_channel::Receiver;
use futures::{StreamExt, channel::oneshot, future, pin_mut};
use presage::{
    Error as PresageError, Manager,
    libsignal_service::{
        configuration::SignalServers,
        content::{Content, ContentBody, DataMessage, GroupContextV2},
        protocol::ServiceId,
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
use sqlx::sqlite::SqliteConnectOptions;
use tokio::{runtime::Runtime, task::LocalSet};
use tracing::{info, warn};

use crate::{
    ChatMessage, Command, Config, Conversation, ConversationId, Event, Events, Password, QrMatrix,
    now_millis,
    vault::{self, KeySource, KeyringKey},
};

type Mgr = Manager<SqliteStore, Registered>;

/// Maps the opaque ids handed to the UI back to presage threads.
type Threads = Rc<RefCell<HashMap<ConversationId, Thread>>>;

pub fn run(config: Config, commands: Receiver<Command>, events: Events) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    let local = tokio::task::LocalSet::new();
    let result = match unlock(&runtime, &local, &commands, &events) {
        Ok(Some(store)) => local.block_on(&runtime, session(config, store, commands, events.clone())),
        // The UI went away while we were waiting for a password.
        Ok(None) => Ok(()),
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        let _ = events.send(Event::Error(error));
    }
}

struct Paths {
    dir: PathBuf,
    db: PathBuf,
    key_source: PathBuf,
}

impl Paths {
    fn new() -> Result<Self, String> {
        let dirs = directories::ProjectDirs::from("org", "langis", "langis")
            .ok_or("could not determine a data directory")?;
        let dir = dirs.data_dir().to_path_buf();
        std::fs::create_dir_all(&dir).map_err(|e| format!("failed to create {}: {e}", dir.display()))?;
        Ok(Self { db: dir.join("langis.db3"), key_source: dir.join("langis.key"), dir })
    }
}

/// Obtains the database key and opens the store; `None` if the UI quit while being asked for
/// a password. Keyring access blocks (D-Bus on Linux), so this runs outside the runtime and
/// only enters it to open the database.
fn unlock(
    runtime: &Runtime,
    local: &LocalSet,
    commands: &Receiver<Command>,
    events: &Events,
) -> Result<Option<SqliteStore>, String> {
    let paths = Paths::new()?;
    let salt = match KeySource::read(&paths.key_source).map_err(|e| e.to_string())? {
        Some(KeySource::Keyring) => {
            let key = match vault::keyring_key() {
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
        None => match vault::create_keyring_key() {
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

fn wait_for_password(commands: &Receiver<Command>) -> Option<Password> {
    // Nothing else can be done before the database is open, so other commands are dropped.
    commands.iter().find_map(|command| match command {
        Command::Unlock(password) => Some(password),
        _ => None,
    })
}

fn open_store(runtime: &Runtime, local: &LocalSet, paths: &Paths, key: &vault::Key) -> Result<SqliteStore, String> {
    let options = SqliteConnectOptions::new()
        .filename(&paths.db)
        .create_if_missing(true)
        .foreign_keys(true)
        // sqlx keeps its own copy of this string; it can't be wiped.
        .pragma("key", vault::sqlcipher_pragma(key).to_string());
    local
        .block_on(runtime, SqliteStore::open_with_options(options, OnNewIdentity::Trust))
        .map_err(|e| format!("failed to open the database: {e}"))
}

async fn session(
    config: Config,
    store: SqliteStore,
    commands: Receiver<Command>,
    events: Events,
) -> Result<(), String> {
    let manager = match Manager::load_registered(store.clone()).await {
        Ok(manager) => manager,
        Err(PresageError::NotYetRegisteredError) => link(store, &config, &events).await?,
        Err(error) => return Err(format!("failed to load registration: {error}")),
    };

    let account = manager
        .registration_data()
        .service_ids
        .aci()
        .service_id_string();
    let _ = events.send(Event::Linked { account });

    let threads: Threads = Rc::default();
    send_conversations(&manager, &threads, &events).await;

    tokio::task::spawn_local(receive(manager.clone(), threads.clone(), events.clone()));

    // crossbeam -> tokio bridge, so the UI side can use plain blocking channels.
    let (command_tx, mut command_rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(command) = commands.recv() {
            if command_tx.send(command).is_err() {
                break;
            }
        }
    });

    let mut manager = manager;
    while let Some(command) = command_rx.recv().await {
        if let Err(error) = handle(&mut manager, &threads, &events, command).await {
            let _ = events.send(Event::Error(error));
        }
    }
    Ok(())
}

async fn link(store: SqliteStore, config: &Config, events: &Events) -> Result<Mgr, String> {
    let (url_tx, url_rx) = oneshot::channel();
    let (manager, _) = future::join(
        Manager::link_secondary_device(
            store,
            SignalServers::Production,
            config.device_name.clone(),
            url_tx,
        ),
        async {
            if let Ok(url) = url_rx.await
                && let Some(qr) = QrMatrix::encode(url.as_str())
            {
                let _ = events.send(Event::LinkQr(qr));
            }
        },
    )
    .await;
    manager.map_err(|e| format!("linking failed: {e}"))
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
async fn send_conversations(manager: &Mgr, threads: &Threads, events: &Events) {
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
    if let Ok(groups) = store.groups().await {
        for (key, group) in groups.flatten() {
            all.push(register(threads, Thread::Group(key), group.title));
        }
    }
    let _ = events.send(Event::Conversations(all));
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

async fn receive(mut manager: Mgr, threads: Threads, events: Events) {
    let messages = match manager.receive_messages().await {
        Ok(messages) => messages,
        Err(error) => {
            let _ = events.send(Event::Error(format!("failed to start receiving: {error}")));
            return;
        }
    };
    pin_mut!(messages);
    let mut names = Names::new();
    while let Some(received) = messages.next().await {
        match received {
            Received::QueueEmpty => {
                let _ = events.send(Event::Synced);
            }
            Received::Contacts => {
                names.clear();
                send_conversations(&manager, &threads, &events).await;
            }
            Received::DecryptionError(sender) => {
                warn!(sender = %sender.service_id_string(), "decryption error")
            }
            Received::Content(content) => {
                let Ok(thread) = Thread::try_from(&*content) else {
                    continue;
                };
                let id = conversation_id(&thread);
                if !threads.borrow().contains_key(&id) {
                    // First message in a conversation we didn't know about yet.
                    let title = manager.thread_title(&thread).await.unwrap_or_default();
                    let conversation = register(&threads, thread, title);
                    info!(title = conversation.title, "new conversation");
                    let _ = events.send(Event::Conversations(vec![conversation]));
                }
                if let Some(message) = to_chat_message(&manager, &mut names, id, &content).await {
                    let _ = events.send(Event::Message(message));
                }
            }
        }
    }
    let _ = events.send(Event::Error("connection to Signal closed".into()));
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
                    message.group_v2 = Some(GroupContextV2 {
                        master_key: Some(key.to_vec()),
                        revision: Some(0),
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

    #[test]
    fn database_opens_only_with_its_key() {
        let dir = std::env::temp_dir().join(format!("langis-db-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths { db: dir.join("test.db3"), key_source: dir.join("test.key"), dir: dir.clone() };
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
