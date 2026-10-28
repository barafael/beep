//! Real backend: presage on a single-threaded tokio runtime (presage's futures are `!Send`).

mod avatars;
mod commands;
mod conversations;
pub(crate) mod errors;
mod groups;
mod history;
mod lifecycle;
mod receive;
mod send;
mod storage_sync;
mod stories;
mod trust;

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, HashSet},
    rc::Rc,
};

use presage::{
    Manager, libsignal_service::protocol::ServiceId, manager::Registered, store::Thread,
};
use presage_store_sqlite::SqliteStore;
use tracing::warn;

use crate::{
    Config, ConversationId, Error, Event, Events, Inbox, Preferences, attachments,
    local::LocalStore, scrub::scrub,
};

pub(crate) use conversations::conversation_id;
use groups::group_identifier;
use lifecycle::{AfterFailure, Ending, Failed, Paths, attempt, wait_after_failure, wipe};

pub(crate) type Mgr = Manager<SqliteStore, Registered>;

/// Maps the opaque ids handed to the UI back to presage threads.
type Threads = Rc<RefCell<HashMap<ConversationId, Thread>>>;

pub fn run(config: Config, commands: Inbox, events: Events) {
    attachments::clean_opened();
    // Kept across unlocking, linking and relinking, which take commands too.
    let preferences = Rc::new(Cell::new(config.preferences));
    let device_name = RefCell::new(config.device_name);
    // crossbeam -> tokio bridge, so the UI side can use plain blocking channels. It ends on
    // shutdown, which closes `commands`.
    let (command_tx, mut commands_rx) = tokio::sync::mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Some(command) = commands.recv() {
            if command_tx.send(command).is_err() {
                break;
            }
        }
    });
    let commands = &mut commands_rx;
    // Held until the backend ends, so a second b33p can't open the same data.
    let mut lock = None;
    let mut use_password = false;
    loop {
        let failed = match attempt(
            &device_name,
            commands,
            &events,
            &preferences,
            &mut lock,
            use_password,
        ) {
            Ok(Ending::Gone) => break,
            Ok(Ending::Wipe) => match Paths::new().and_then(|paths| wipe(&paths)) {
                Ok(()) => {
                    use_password = false;
                    continue;
                }
                Err(message) => Failed::other(message),
            },
            Err(failed) => failed,
        };
        warn!(failure = ?failed.failure, reason = %scrub(&failed.detail), "the backend can't go on");
        let failure = failed.failure;
        if !events.sent(Event::Failed {
            failure,
            message: failed.message,
        }) {
            break;
        }
        match wait_after_failure(commands, &preferences, &device_name, failure) {
            AfterFailure::Retry => {}
            AfterFailure::StartOver => match Paths::new().and_then(|paths| wipe(&paths)) {
                Ok(()) => use_password = false,
                Err(message) => {
                    events.send(Event::Error(Error::storage(message)));
                }
            },
            AfterFailure::UsePassword => use_password = true,
            AfterFailure::Gone => break,
        }
    }
    attachments::clean_opened();
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
    /// Whether the phone's settings (who was accepted, who is blocked) have been read from the
    /// storage service: not yet right after linking.
    storage_read: Rc<tokio::sync::watch::Sender<bool>>,
    /// How far each contact's safety number is trusted, as the UI was last told; see
    /// [`publish_trust`].
    trust: Rc<RefCell<HashMap<ServiceId, crate::Trust>>>,
    group_ids: GroupIds,
    background: Background,
    /// Every send goes through here, one at a time ([`outbox`](crate::outbox)).
    outbox: crate::outbox::Outbox,
}

/// Commands that read a lot (a chat's whole history, every sticker) and so run beside the
/// command loop rather than hold up what comes after them. A newer one of a kind makes the
/// older one pointless (the UI shows only the answer to the latest), so it stops that.
#[derive(Clone, Default)]
struct Background(Rc<RefCell<HashMap<BackgroundKind, tokio::task::AbortHandle>>>);

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum BackgroundKind {
    Search,
    Media,
    Stickers,
}

impl Background {
    /// Runs `task` beside the command loop, stopping the one of its kind still running.
    fn run(&self, kind: BackgroundKind, task: impl Future<Output = ()> + 'static) {
        if let Some(older) = self.0.borrow_mut().remove(&kind) {
            older.abort();
        }
        let task = tokio::task::spawn_local(task);
        self.0.borrow_mut().insert(kind, task.abort_handle());
    }

    /// Stops them all, at the end of the session.
    fn stop(&self) {
        for (_, task) in self.0.borrow_mut().drain() {
            task.abort();
        }
    }
}

/// The groups of [`Ctx::threads`] by the id typing indicators and sync messages name them by
/// (see [`group_identifier`]), each worked out once: that is elliptic-curve arithmetic, too
/// much to do for every group whenever someone types.
#[derive(Clone, Default)]
struct GroupIds(Rc<RefCell<GroupIdIndex>>);

#[derive(Default)]
struct GroupIdIndex {
    /// Master key by group id.
    keys: HashMap<[u8; 32], [u8; 32]>,
    /// The master keys in `keys`.
    known: HashSet<[u8; 32]>,
}

impl GroupIds {
    /// The group in `threads` that `group_id` names.
    fn find(&self, threads: &Threads, group_id: &[u8]) -> Option<Thread> {
        let mut index = self.0.borrow_mut();
        let GroupIdIndex { keys, known } = &mut *index;
        if !keys.contains_key(group_id) {
            // Groups registered since the last look, if any.
            for thread in threads.borrow().values() {
                if let Thread::Group(key) = thread
                    && known.insert(*key)
                {
                    keys.insert(group_identifier(key), *key);
                }
            }
        }
        let thread = Thread::Group(*keys.get(group_id)?);
        threads
            .borrow()
            .contains_key(&conversation_id(&thread))
            .then_some(thread)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signal::conversations::register;
    use crate::signal::lifecycle::tests::{open_and_close, test_paths};
    use crate::vault;
    use std::time::Duration;
    use tokio::task::LocalSet;

    pub(super) fn aci(n: u8) -> presage::libsignal_service::protocol::Aci {
        presage::libsignal_service::protocol::Aci::from(
            presage::libsignal_service::prelude::Uuid::from_u128(n.into()),
        )
    }

    /// presage's migrations as this build has them: a version and its checksum per line. Users'
    /// databases record each migration they ran, and sqlx refuses a database whose migrations
    /// were changed or dropped since (see `a_database_from_a_newer_version_says_so`). Rebuilding
    /// the presage fork's integration branch must therefore only ever add migrations at the end:
    /// a line here that changes means existing users would be locked out. New lines are added
    /// to the file on purpose, where review sees them.
    #[test]
    fn presage_migrations_only_ever_grow() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = test_paths(tmp.path());
        let key = vault::password_key("correct horse", &[3; 16], vault::Kdf::V1);
        let mut applied = String::new();
        open_and_close(&paths, &key, async |_, tables| {
            let rows: Vec<(i64, Vec<u8>)> =
                sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations ORDER BY version")
                    .fetch_all(tables.pool())
                    .await
                    .unwrap();
            for (version, checksum) in rows {
                applied += &format!("{version} {}\n", hex::encode(checksum));
            }
        })
        .unwrap();
        let known = include_str!("../tests/presage-migrations.txt");
        for line in known.lines() {
            assert!(
                applied.lines().any(|applied| applied == line),
                "presage migration changed or gone: {line}\nnow:\n{applied}"
            );
        }
        assert_eq!(
            applied, known,
            "new presage migrations: add them to tests/presage-migrations.txt"
        );
    }

    /// A newer background command stops the older one of its kind, and only that one; the end
    /// of the session stops them all.
    #[test]
    fn a_newer_background_command_stops_the_older() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        LocalSet::new().block_on(&runtime, async {
            let background = Background::default();
            let finished = Rc::new(RefCell::new(Vec::new()));
            let task = |name: &'static str, millis: u64| {
                let finished = finished.clone();
                async move {
                    tokio::time::sleep(Duration::from_millis(millis)).await;
                    finished.borrow_mut().push(name);
                }
            };
            background.run(BackgroundKind::Search, task("old search", 50));
            background.run(BackgroundKind::Media, task("media", 50));
            tokio::task::yield_now().await;
            background.run(BackgroundKind::Search, task("new search", 10));
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert_eq!(*finished.borrow(), ["new search", "media"]);
            background.run(BackgroundKind::Stickers, task("stickers", 10));
            background.stop();
            tokio::time::sleep(Duration::from_millis(30)).await;
            assert_eq!(finished.borrow().len(), 2);
        });
    }

    /// Groups are found by the id typing indicators name them by, also those registered after
    /// the first look; contacts and unknown groups aren't.
    #[test]
    fn groups_are_found_by_their_id() {
        let threads = Threads::default();
        let ids = GroupIds::default();
        let ann = Thread::Contact(ServiceId::Aci(aci(1)));
        for thread in [ann, Thread::Group([1; 32])] {
            register(&threads, thread, String::new(), String::new(), 0);
        }
        let found = |key: [u8; 32]| ids.find(&threads, &group_identifier(&key));
        assert_eq!(found([1; 32]), Some(Thread::Group([1; 32])));
        assert_eq!(found([2; 32]), None);
        register(
            &threads,
            Thread::Group([2; 32]),
            String::new(),
            String::new(),
            0,
        );
        assert_eq!(found([2; 32]), Some(Thread::Group([2; 32])));
        assert_eq!(found([1; 32]), Some(Thread::Group([1; 32])));
        assert_eq!(ids.find(&threads, b"short"), None);
    }
}
