//! b33p's own data, kept beside presage's in the same encrypted database: what the protocol
//! doesn't carry or presage doesn't keep. Pins, mutes, the archive and read positions per
//! conversation; delivery states, edit history, deletion markers and expiry times per message.
//!
//! It uses its own connection pool, opened with the same options (key, close guard) as the
//! store's, so closing waits for both and nothing is ever written unencrypted.

use std::collections::HashMap;

use sqlx::{
    Row as _, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow},
};

use presage::proto::verified::State;

use crate::{ChatSettings, ConversationId, Delivery, MessageRef};

/// Whether a contact's safety number was verified, and for which of their identity keys
/// (serialized), in Signal's terms: [`State::Default`] (not verified, or cleared),
/// [`State::Verified`] (only that key), or [`State::Unverified`] (verified before, changed
/// since, and not confirmed yet: nothing is sent to them).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Verification {
    pub identity_key: Vec<u8>,
    pub state: State,
}

/// A verification as kept in its tables: `identity_key` and `verified` (Signal's state as a
/// number) at `at` and the column after.
fn verification_of(row: &SqliteRow, at: usize) -> Verification {
    Verification {
        identity_key: row.get(at),
        state: State::try_from(row.get::<i64, _>(at + 1) as i32).unwrap_or(State::Default),
    }
}

/// A story as kept: a picture (`pointer`, an encoded attachment pointer) or text.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct StoredStory {
    pub author: String,
    pub timestamp: u64,
    pub conversation: ConversationId,
    pub pointer: Option<Vec<u8>>,
    pub text: String,
    pub foreground: u32,
    pub background: u32,
    pub viewed: bool,
    pub outgoing: bool,
}

#[derive(Clone)]
pub(crate) struct LocalStore {
    db: SqlitePool,
}

/// A message as identified in b33p's tables.
fn key(conversation: &ConversationId, message: &MessageRef) -> (String, String, i64) {
    (
        conversation.0.clone(),
        message.author_id.clone(),
        message.timestamp as i64,
    )
}

/// The message whose author and time are in the columns from `first` on.
fn message_at(row: &SqliteRow, first: usize) -> MessageRef {
    MessageRef {
        author_id: row.get(first),
        timestamp: row.get::<i64, _>(first + 1) as u64,
    }
}

/// The columns [`settings_of`] reads, in its order, after the conversation id.
macro_rules! settings {
    () => {
        "id, pinned, muted_until, archived, last_read, marked_unread, blocked, accepted"
    };
}

fn settings_of(row: &SqliteRow) -> ChatSettings {
    ChatSettings {
        pinned: row.get::<Option<i64>, _>(1).map(|p| p as u32),
        muted_until: row.get::<Option<i64>, _>(2).map(|m| m as u64),
        archived: row.get::<i64, _>(3) != 0,
        last_read: row.get::<i64, _>(4) as u64,
        marked_unread: row.get::<i64, _>(5) != 0,
        blocked: row.get::<i64, _>(6) != 0,
        accepted: row.get::<i64, _>(7) != 0,
    }
}

/// The version of b33p's tables this build writes, kept in `b33p_meta` as `schema`. Changes
/// that older builds read correctly ([`SCHEMA`]'s `IF NOT EXISTS` tables, [`ADDED_COLUMNS`])
/// keep it; one they would misread raises it, so that they refuse the database instead.
const SCHEMA_VERSION: i64 = 1;

/// Why [`LocalStore::open`] failed.
#[derive(Debug)]
pub(crate) enum OpenError {
    Db(sqlx::Error),
    /// Written by a newer b33p, at this [`SCHEMA_VERSION`].
    TooNew(i64),
}

impl From<sqlx::Error> for OpenError {
    fn from(error: sqlx::Error) -> Self {
        Self::Db(error)
    }
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Db(error) => error.fmt(f),
            Self::TooNew(version) => write!(f, "b33p's tables are at version {version}"),
        }
    }
}

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS b33p_conversation (
        id TEXT PRIMARY KEY NOT NULL,
        pinned INTEGER,
        muted_until INTEGER,
        archived INTEGER NOT NULL DEFAULT 0,
        last_read INTEGER NOT NULL DEFAULT 0,
        marked_unread INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS b33p_delivery (
        conversation TEXT NOT NULL,
        ts INTEGER NOT NULL,
        state INTEGER NOT NULL,
        PRIMARY KEY (conversation, ts)
    )",
    // Receipts name only the time.
    "CREATE INDEX IF NOT EXISTS b33p_delivery_ts ON b33p_delivery (ts)",
    "CREATE TABLE IF NOT EXISTS b33p_edit (
        conversation TEXT NOT NULL,
        author TEXT NOT NULL,
        ts INTEGER NOT NULL,
        edited_at INTEGER NOT NULL,
        previous_body TEXT NOT NULL,
        PRIMARY KEY (conversation, author, ts, edited_at)
    )",
    "CREATE TABLE IF NOT EXISTS b33p_deleted (
        conversation TEXT NOT NULL,
        author TEXT NOT NULL,
        ts INTEGER NOT NULL,
        PRIMARY KEY (conversation, author, ts)
    )",
    "CREATE TABLE IF NOT EXISTS b33p_expiry (
        conversation TEXT NOT NULL,
        author TEXT NOT NULL,
        ts INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        PRIMARY KEY (conversation, author, ts)
    )",
    "CREATE INDEX IF NOT EXISTS b33p_expiry_at ON b33p_expiry (expires_at)",
    // What the storage service said last, per conversation; see `storage.rs`. `accepted` is
    // its "whitelisted".
    "CREATE TABLE IF NOT EXISTS b33p_remote (
        id TEXT PRIMARY KEY NOT NULL,
        pinned INTEGER,
        muted_until INTEGER,
        archived INTEGER NOT NULL DEFAULT 0,
        marked_unread INTEGER NOT NULL DEFAULT 0
    )",
    // The phone's chat folders, in order; conversation ids one per line.
    "CREATE TABLE IF NOT EXISTS b33p_folder (
        position INTEGER PRIMARY KEY NOT NULL,
        name TEXT NOT NULL,
        only_unread INTEGER NOT NULL,
        show_muted INTEGER NOT NULL,
        all_individual INTEGER NOT NULL,
        all_groups INTEGER NOT NULL,
        included TEXT NOT NULL,
        excluded TEXT NOT NULL
    )",
    // Stories, kept for a day: presage doesn't keep them. `kind` 0 is a picture (`pointer` an
    // encoded attachment pointer), 1 text.
    "CREATE TABLE IF NOT EXISTS b33p_story (
        author TEXT NOT NULL,
        ts INTEGER NOT NULL,
        conversation TEXT NOT NULL,
        kind INTEGER NOT NULL,
        text TEXT NOT NULL,
        foreground INTEGER NOT NULL,
        background INTEGER NOT NULL,
        pointer BLOB,
        viewed INTEGER NOT NULL DEFAULT 0,
        outgoing INTEGER NOT NULL DEFAULT 0,
        PRIMARY KEY (author, ts)
    )",
    // Whether we verified a contact's safety number, and for which of their identity keys: a new
    // key isn't verified. By the contact's ACI; `verified` is Signal's state (0 default,
    // 1 verified, 2 unverified: changed since verified, not confirmed).
    "CREATE TABLE IF NOT EXISTS b33p_verification (
        aci TEXT PRIMARY KEY NOT NULL,
        identity_key BLOB NOT NULL,
        verified INTEGER NOT NULL
    )",
    // The same as the storage service said it last, to tell its changes from ours.
    "CREATE TABLE IF NOT EXISTS b33p_remote_verification (
        aci TEXT PRIMARY KEY NOT NULL,
        identity_key BLOB NOT NULL,
        verified INTEGER NOT NULL
    )",
    "CREATE TABLE IF NOT EXISTS b33p_meta (
        key TEXT PRIMARY KEY NOT NULL,
        value INTEGER NOT NULL
    )",
];

/// Columns added to tables after they first shipped: table, column, and how to add it.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    (
        "b33p_conversation",
        "blocked",
        "ALTER TABLE b33p_conversation ADD COLUMN blocked INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "b33p_conversation",
        "accepted",
        "ALTER TABLE b33p_conversation ADD COLUMN accepted INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "b33p_remote",
        "blocked",
        "ALTER TABLE b33p_remote ADD COLUMN blocked INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "b33p_remote",
        "accepted",
        "ALTER TABLE b33p_remote ADD COLUMN accepted INTEGER NOT NULL DEFAULT 0",
    ),
];

impl LocalStore {
    /// Begins a transaction that writes, taking the write lock at once: presage writes to the
    /// same database, and a deferred transaction that reads before it writes fails with
    /// `SQLITE_BUSY`, which no busy timeout waits out, when it writes in between.
    async fn begin_write(&self) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>, sqlx::Error> {
        self.db.begin_with("BEGIN IMMEDIATE").await
    }

    /// The database underneath, for tests that set it up as something else would leave it.
    #[cfg(test)]
    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.db
    }

    /// Releases the database file, which Windows won't let a test delete while it is open.
    #[cfg(test)]
    pub(crate) async fn close(&self) {
        self.db.close().await;
    }

    pub(crate) async fn open(options: SqliteConnectOptions) -> Result<Self, OpenError> {
        // Little traffic: a couple of connections are plenty.
        let db = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await?;
        // Before changing anything: a newer b33p may have changed the tables in ways this one
        // would get wrong.
        let found: Option<i64> =
            sqlx::query_scalar("SELECT value FROM b33p_meta WHERE key = 'schema'")
                .fetch_optional(&db)
                .await
                .or_else(|error| match &error {
                    // Made before the version was kept, or not at all yet.
                    sqlx::Error::Database(e) if e.message().contains("no such table") => Ok(None),
                    _ => Err(error),
                })?;
        if let Some(found) = found
            && found > SCHEMA_VERSION
        {
            return Err(OpenError::TooNew(found));
        }
        for statement in SCHEMA {
            sqlx::query(*statement).execute(&db).await?;
        }
        // Columns added since the tables were first made.
        for (table, column, statement) in ADDED_COLUMNS {
            let exists: Option<i64> =
                sqlx::query_scalar("SELECT 1 FROM pragma_table_info(?) WHERE name = ?")
                    .bind(table)
                    .bind(column)
                    .fetch_optional(&db)
                    .await?;
            if exists.is_none() {
                sqlx::query(*statement).execute(&db).await?;
            }
        }
        sqlx::query(
            "INSERT INTO b33p_meta (key, value) VALUES ('schema', ?)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        )
        .bind(SCHEMA_VERSION)
        .execute(&db)
        .await?;
        Ok(Self { db })
    }

    /// Forgets everything b33p keeps here, for a database whose contents are wiped too.
    pub(crate) async fn clear(&self) -> Result<(), sqlx::Error> {
        let mut tx = self.begin_write().await?;
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'b33p\\_%' ESCAPE '\\'",
        )
        .fetch_all(&mut *tx)
        .await?;
        for table in tables {
            // From `sqlite_master`, and b33p's own: safe to put into the statement.
            sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM \"{table}\"")))
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await
    }

    /// Conversations b33p remembers something about, by id.
    pub(crate) async fn conversations(
        &self,
    ) -> Result<HashMap<ConversationId, ChatSettings>, sqlx::Error> {
        let rows = sqlx::query(concat!("SELECT ", settings!(), " FROM b33p_conversation"))
            .fetch_all(&self.db)
            .await?;
        Ok(rows
            .iter()
            .map(|row| (ConversationId(row.get(0)), settings_of(row)))
            .collect())
    }

    /// What b33p remembers about one conversation, if anything.
    pub(crate) async fn settings(
        &self,
        id: &ConversationId,
    ) -> Result<Option<ChatSettings>, sqlx::Error> {
        settings_in(&self.db, id).await
    }

    /// Changes what b33p remembers about one conversation. The read and the write are one
    /// transaction: another change in between (`mark_read` for nearly every message) would be
    /// written over with what was read before it.
    pub(crate) async fn update(
        &self,
        id: &ConversationId,
        change: impl FnOnce(&mut ChatSettings),
    ) -> Result<ChatSettings, sqlx::Error> {
        let mut tx = self.begin_write().await?;
        let mut state = settings_in(&mut *tx, id).await?.unwrap_or_default();
        change(&mut state);
        sqlx::query(
            "INSERT OR REPLACE INTO b33p_conversation
                (id, pinned, muted_until, archived, last_read, marked_unread, blocked, accepted)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id.0)
        .bind(state.pinned.map(i64::from))
        .bind(state.muted_until.map(|m| m.min(i64::MAX as u64) as i64))
        .bind(state.archived as i64)
        .bind(state.last_read as i64)
        .bind(state.marked_unread as i64)
        .bind(state.blocked as i64)
        .bind(state.accepted as i64)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(state)
    }

    /// Incoming messages up to `newest` were read, and the conversation no longer marked
    /// unread. One statement: this runs for nearly every message in an open chat.
    pub(crate) async fn mark_read(
        &self,
        id: &ConversationId,
        newest: u64,
    ) -> Result<ChatSettings, sqlx::Error> {
        let row = sqlx::query(concat!(
            "INSERT INTO b33p_conversation (id, last_read) VALUES (?, ?)
             ON CONFLICT(id) DO UPDATE SET
                last_read = max(last_read, excluded.last_read), marked_unread = 0
             RETURNING ",
            settings!()
        ))
        .bind(&id.0)
        .bind(newest as i64)
        .fetch_one(&self.db)
        .await?;
        Ok(settings_of(&row))
    }

    /// Takes a conversation out of the archive unless it is muted at `now`; its settings if
    /// that changed anything.
    pub(crate) async fn unarchive_unless_muted(
        &self,
        id: &ConversationId,
        now: u64,
    ) -> Result<Option<ChatSettings>, sqlx::Error> {
        let row = sqlx::query(concat!(
            "UPDATE b33p_conversation SET archived = 0
             WHERE id = ? AND archived = 1 AND (muted_until IS NULL OR muted_until <= ?)
             RETURNING ",
            settings!()
        ))
        .bind(&id.0)
        .bind(now.min(i64::MAX as u64) as i64)
        .fetch_optional(&self.db)
        .await?;
        Ok(row.as_ref().map(settings_of))
    }

    /// Stores several conversations' settings at once (but their read positions).
    pub(crate) async fn save_settings(
        &self,
        settings: &[(ConversationId, ChatSettings)],
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.begin_write().await?;
        for (id, s) in settings {
            sqlx::query(
                "INSERT INTO b33p_conversation
                    (id, pinned, muted_until, archived, marked_unread, blocked, accepted)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET pinned = excluded.pinned,
                    muted_until = excluded.muted_until, archived = excluded.archived,
                    marked_unread = excluded.marked_unread, blocked = excluded.blocked,
                    accepted = excluded.accepted",
            )
            .bind(&id.0)
            .bind(s.pinned.map(i64::from))
            .bind(s.muted_until.map(|m| m.min(i64::MAX as u64) as i64))
            .bind(s.archived as i64)
            .bind(s.marked_unread as i64)
            .bind(s.blocked as i64)
            .bind(s.accepted as i64)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }

    /// What the storage service said last (`last_read` unused), by conversation.
    pub(crate) async fn remote(
        &self,
    ) -> Result<HashMap<ConversationId, ChatSettings>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, pinned, muted_until, archived, 0, marked_unread, blocked, accepted
             FROM b33p_remote",
        )
        .fetch_all(&self.db)
        .await?;
        Ok(rows
            .iter()
            .map(|row| (ConversationId(row.get(0)), settings_of(row)))
            .collect())
    }

    /// Replaces what the storage service said last, and its manifest version.
    pub(crate) async fn set_remote(
        &self,
        remote: &HashMap<ConversationId, ChatSettings>,
        version: u64,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.begin_write().await?;
        sqlx::query("DELETE FROM b33p_remote")
            .execute(&mut *tx)
            .await?;
        for (id, s) in remote {
            sqlx::query(
                "INSERT INTO b33p_remote
                    (id, pinned, muted_until, archived, marked_unread, blocked, accepted)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id.0)
            .bind(s.pinned.map(i64::from))
            .bind(s.muted_until.map(|m| m.min(i64::MAX as u64) as i64))
            .bind(s.archived as i64)
            .bind(s.marked_unread as i64)
            .bind(s.blocked as i64)
            .bind(s.accepted as i64)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "INSERT INTO b33p_meta (key, value) VALUES ('storage_version', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(version.min(i64::MAX as u64) as i64)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }

    /// Keeps a story (once).
    pub(crate) async fn add_story(&self, story: &StoredStory) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT OR IGNORE INTO b33p_story (author, ts, conversation, kind, text, foreground,
                background, pointer, viewed, outgoing)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&story.author)
        .bind(story.timestamp as i64)
        .bind(&story.conversation.0)
        .bind(i64::from(story.pointer.is_none()))
        .bind(&story.text)
        .bind(i64::from(story.foreground))
        .bind(i64::from(story.background))
        .bind(story.pointer.as_deref())
        .bind(story.viewed as i64)
        .bind(story.outgoing as i64)
        .execute(&self.db)
        .await
        .map(drop)
    }

    /// The stories since `since` (ms), oldest first; older ones are forgotten.
    pub(crate) async fn stories(&self, since: u64) -> Result<Vec<StoredStory>, sqlx::Error> {
        sqlx::query("DELETE FROM b33p_story WHERE ts < ?")
            .bind(since as i64)
            .execute(&self.db)
            .await?;
        let rows = sqlx::query(
            "SELECT author, ts, conversation, kind, text, foreground, background, pointer, viewed,
                outgoing
             FROM b33p_story ORDER BY ts",
        )
        .fetch_all(&self.db)
        .await?;
        Ok(rows
            .iter()
            .map(|row| StoredStory {
                author: row.get(0),
                timestamp: row.get::<i64, _>(1) as u64,
                conversation: ConversationId(row.get(2)),
                pointer: (row.get::<i64, _>(3) == 0).then(|| row.get(7)),
                text: row.get(4),
                foreground: row.get::<i64, _>(5) as u32,
                background: row.get::<i64, _>(6) as u32,
                viewed: row.get::<i64, _>(8) != 0,
                outgoing: row.get::<i64, _>(9) != 0,
            })
            .collect())
    }

    pub(crate) async fn story_viewed(&self, story: &MessageRef) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE b33p_story SET viewed = 1 WHERE author = ? AND ts = ?")
            .bind(&story.author_id)
            .bind(story.timestamp as i64)
            .execute(&self.db)
            .await
            .map(drop)
    }

    /// The chat folders, in order.
    pub(crate) async fn folders(&self) -> Result<Vec<crate::ChatFolder>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT name, only_unread, show_muted, all_individual, all_groups, included, excluded
             FROM b33p_folder ORDER BY position",
        )
        .fetch_all(&self.db)
        .await?;
        let ids = |text: String| -> Vec<ConversationId> {
            text.lines()
                .map(|id| ConversationId(id.to_owned()))
                .collect()
        };
        Ok(rows
            .into_iter()
            .map(|row| crate::ChatFolder {
                name: row.get(0),
                only_unread: row.get::<i64, _>(1) != 0,
                show_muted: row.get::<i64, _>(2) != 0,
                all_individual: row.get::<i64, _>(3) != 0,
                all_groups: row.get::<i64, _>(4) != 0,
                included: ids(row.get(5)),
                excluded: ids(row.get(6)),
            })
            .collect())
    }

    /// Replaces the chat folders.
    pub(crate) async fn set_folders(
        &self,
        folders: &[crate::ChatFolder],
    ) -> Result<(), sqlx::Error> {
        let ids = |ids: &[ConversationId]| -> String {
            ids.iter()
                .map(|id| id.0.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let mut tx = self.begin_write().await?;
        sqlx::query("DELETE FROM b33p_folder")
            .execute(&mut *tx)
            .await?;
        for (position, folder) in folders.iter().enumerate() {
            sqlx::query(
                "INSERT INTO b33p_folder (position, name, only_unread, show_muted,
                    all_individual, all_groups, included, excluded)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(position as i64)
            .bind(&folder.name)
            .bind(folder.only_unread as i64)
            .bind(folder.show_muted as i64)
            .bind(folder.all_individual as i64)
            .bind(folder.all_groups as i64)
            .bind(ids(&folder.included))
            .bind(ids(&folder.excluded))
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }

    /// The storage service's manifest version last read; 0 before the first time.
    pub(crate) async fn storage_version(&self) -> Result<u64, sqlx::Error> {
        let version: Option<i64> =
            sqlx::query_scalar("SELECT value FROM b33p_meta WHERE key = 'storage_version'")
                .fetch_optional(&self.db)
                .await?;
        Ok(version.unwrap_or(0) as u64)
    }

    /// Whether we verified the contact with this ACI, and their key then.
    pub(crate) async fn verification(
        &self,
        aci: &str,
    ) -> Result<Option<Verification>, sqlx::Error> {
        let row = sqlx::query("SELECT identity_key, verified FROM b33p_verification WHERE aci = ?")
            .bind(aci)
            .fetch_optional(&self.db)
            .await?;
        Ok(row.map(|row| verification_of(&row, 0)))
    }

    /// Every contact we verified or cleared the verification of, by ACI.
    pub(crate) async fn verifications(&self) -> Result<HashMap<String, Verification>, sqlx::Error> {
        let rows = sqlx::query("SELECT aci, identity_key, verified FROM b33p_verification")
            .fetch_all(&self.db)
            .await?;
        Ok(rows
            .iter()
            .map(|row| (row.get(0), verification_of(row, 1)))
            .collect())
    }

    /// Remembers whether we verified the contact with this ACI, for this key of theirs.
    pub(crate) async fn set_verification(
        &self,
        aci: &str,
        verification: &Verification,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT OR REPLACE INTO b33p_verification (aci, identity_key, verified)
             VALUES (?, ?, ?)",
        )
        .bind(aci)
        .bind(&verification.identity_key)
        .bind(verification.state as i64)
        .execute(&self.db)
        .await
        .map(drop)
    }

    /// What the storage service said last about verifications, by ACI.
    pub(crate) async fn remote_verifications(
        &self,
    ) -> Result<HashMap<String, Verification>, sqlx::Error> {
        let rows = sqlx::query("SELECT aci, identity_key, verified FROM b33p_remote_verification")
            .fetch_all(&self.db)
            .await?;
        Ok(rows
            .iter()
            .map(|row| (row.get(0), verification_of(row, 1)))
            .collect())
    }

    /// Whether what the storage service says about verifications was ever read: until then,
    /// the whole manifest is read again even if it didn't change.
    pub(crate) async fn remote_verifications_read(&self) -> Result<bool, sqlx::Error> {
        let read: Option<i64> =
            sqlx::query_scalar("SELECT value FROM b33p_meta WHERE key = 'remote_verifications'")
                .fetch_optional(&self.db)
                .await?;
        Ok(read.is_some())
    }

    /// Replaces what the storage service said last about verifications.
    pub(crate) async fn set_remote_verifications(
        &self,
        remote: &HashMap<String, Verification>,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.begin_write().await?;
        sqlx::query("DELETE FROM b33p_remote_verification")
            .execute(&mut *tx)
            .await?;
        for (aci, verification) in remote {
            sqlx::query(
                "INSERT INTO b33p_remote_verification (aci, identity_key, verified)
                 VALUES (?, ?, ?)",
            )
            .bind(aci)
            .bind(&verification.identity_key)
            .bind(verification.state as i64)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "INSERT INTO b33p_meta (key, value) VALUES ('remote_verifications', 1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }

    /// Pins exactly `order`, in that order; everything else is unpinned.
    pub(crate) async fn set_pins(&self, order: &[ConversationId]) -> Result<(), sqlx::Error> {
        let mut tx = self.begin_write().await?;
        sqlx::query("UPDATE b33p_conversation SET pinned = NULL")
            .execute(&mut *tx)
            .await?;
        // As `ChatSettings::set_pinned`: pinned chats aren't archived.
        for (position, id) in order.iter().enumerate() {
            sqlx::query("INSERT INTO b33p_conversation (id, pinned) VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET pinned = excluded.pinned, archived = 0")
                .bind(&id.0)
                .bind(position as i64)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await
    }

    /// Records how far one of our messages got; never goes backwards (a late delivery receipt
    /// doesn't undo a read one).
    pub(crate) async fn set_delivery(
        &self,
        conversation: &ConversationId,
        ts: u64,
        delivery: Delivery,
    ) -> Result<(), sqlx::Error> {
        self.set_deliveries(&[(conversation.clone(), ts)], delivery)
            .await
    }

    /// [`LocalStore::set_delivery`] for several messages, in one transaction.
    pub(crate) async fn set_deliveries(
        &self,
        messages: &[(ConversationId, u64)],
        delivery: Delivery,
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.begin_write().await?;
        for (conversation, ts) in messages {
            sqlx::query(
                "INSERT INTO b33p_delivery (conversation, ts, state) VALUES (?, ?, ?)
                 ON CONFLICT(conversation, ts) DO UPDATE SET state = max(state, excluded.state)",
            )
            .bind(&conversation.0)
            .bind(*ts as i64)
            .bind(delivery as i64)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }

    /// Delivery states of our messages in a conversation, by timestamp.
    pub(crate) async fn deliveries(
        &self,
        conversation: &ConversationId,
    ) -> Result<HashMap<u64, Delivery>, sqlx::Error> {
        let rows = sqlx::query("SELECT ts, state FROM b33p_delivery WHERE conversation = ?")
            .bind(&conversation.0)
            .fetch_all(&self.db)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                (
                    row.get::<i64, _>(0) as u64,
                    Delivery::from_index(row.get(1)),
                )
            })
            .collect())
    }

    /// Our messages (anywhere) sent at one of `timestamps`, with their conversation: receipts
    /// name only the timestamps.
    pub(crate) async fn conversations_of(
        &self,
        timestamps: &[u64],
    ) -> Result<Vec<(ConversationId, u64)>, sqlx::Error> {
        let mut found = Vec::new();
        for ts in timestamps {
            let rows = sqlx::query("SELECT conversation FROM b33p_delivery WHERE ts = ?")
                .bind(*ts as i64)
                .fetch_all(&self.db)
                .await?;
            found.extend(
                rows.into_iter()
                    .map(|row| (ConversationId(row.get(0)), *ts)),
            );
        }
        Ok(found)
    }

    /// Keeps the text a message had before an edit.
    pub(crate) async fn record_edit(
        &self,
        conversation: &ConversationId,
        message: &MessageRef,
        edited_at: u64,
        previous_body: &str,
    ) -> Result<(), sqlx::Error> {
        let (conversation, author, ts) = key(conversation, message);
        sqlx::query("INSERT OR REPLACE INTO b33p_edit (conversation, author, ts, edited_at, previous_body) VALUES (?, ?, ?, ?, ?)")
            .bind(conversation)
            .bind(author)
            .bind(ts)
            .bind(edited_at as i64)
            .bind(previous_body)
            .execute(&self.db)
            .await
            .map(drop)
    }

    /// Earlier versions of edited messages in a conversation, oldest first.
    pub(crate) async fn edits(
        &self,
        conversation: &ConversationId,
    ) -> Result<HashMap<MessageRef, Vec<(u64, String)>>, sqlx::Error> {
        let rows = sqlx::query("SELECT author, ts, edited_at, previous_body FROM b33p_edit WHERE conversation = ? ORDER BY edited_at")
            .bind(&conversation.0)
            .fetch_all(&self.db)
            .await?;
        let mut edits: HashMap<MessageRef, Vec<(u64, String)>> = HashMap::new();
        for row in rows {
            edits
                .entry(message_at(&row, 0))
                .or_default()
                .push((row.get::<i64, _>(2) as u64, row.get(3)));
        }
        Ok(edits)
    }

    /// Remembers that a message was deleted, so it keeps showing as such.
    pub(crate) async fn record_deleted(
        &self,
        conversation: &ConversationId,
        message: &MessageRef,
    ) -> Result<(), sqlx::Error> {
        let (conversation, author, ts) = key(conversation, message);
        let mut tx = self.begin_write().await?;
        sqlx::query(
            "INSERT OR IGNORE INTO b33p_deleted (conversation, author, ts) VALUES (?, ?, ?)",
        )
        .bind(&conversation)
        .bind(&author)
        .bind(ts)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM b33p_edit WHERE conversation = ? AND author = ? AND ts = ?")
            .bind(&conversation)
            .bind(&author)
            .bind(ts)
            .execute(&mut *tx)
            .await?;
        tx.commit().await
    }

    pub(crate) async fn deleted(
        &self,
        conversation: &ConversationId,
    ) -> Result<Vec<MessageRef>, sqlx::Error> {
        let rows = sqlx::query("SELECT author, ts FROM b33p_deleted WHERE conversation = ?")
            .bind(&conversation.0)
            .fetch_all(&self.db)
            .await?;
        Ok(rows.iter().map(|row| message_at(row, 0)).collect())
    }

    /// When disappearing messages go; set once each (the first start counts).
    pub(crate) async fn set_expiries(
        &self,
        conversation: &ConversationId,
        expiries: &[(MessageRef, u64)],
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.begin_write().await?;
        for (message, expires_at) in expiries {
            let (conversation, author, ts) = key(conversation, message);
            sqlx::query("INSERT OR IGNORE INTO b33p_expiry (conversation, author, ts, expires_at) VALUES (?, ?, ?, ?)")
                .bind(conversation)
                .bind(author)
                .bind(ts)
                .bind(*expires_at as i64)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await
    }

    /// Expiry times in a conversation.
    pub(crate) async fn expiries(
        &self,
        conversation: &ConversationId,
    ) -> Result<HashMap<MessageRef, u64>, sqlx::Error> {
        let rows =
            sqlx::query("SELECT author, ts, expires_at FROM b33p_expiry WHERE conversation = ?")
                .bind(&conversation.0)
                .fetch_all(&self.db)
                .await?;
        Ok(rows
            .into_iter()
            .map(|row| (message_at(&row, 0), row.get::<i64, _>(2) as u64))
            .collect())
    }

    /// Messages whose time ran out by `now`, and the next time one will.
    pub(crate) async fn due(
        &self,
        now: u64,
    ) -> Result<(Vec<(ConversationId, MessageRef)>, Option<u64>), sqlx::Error> {
        let due =
            sqlx::query("SELECT conversation, author, ts FROM b33p_expiry WHERE expires_at <= ?")
                .bind(now as i64)
                .fetch_all(&self.db)
                .await?
                .into_iter()
                .map(|row| (ConversationId(row.get(0)), message_at(&row, 1)))
                .collect();
        let next: Option<i64> =
            sqlx::query("SELECT min(expires_at) FROM b33p_expiry WHERE expires_at > ?")
                .bind(now as i64)
                .fetch_one(&self.db)
                .await?
                .get(0);
        Ok((due, next.map(|n| n as u64)))
    }

    /// Forgets messages gone from the store, in one transaction.
    pub(crate) async fn forget(
        &self,
        messages: &[(ConversationId, MessageRef)],
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.begin_write().await?;
        for (conversation, message) in messages {
            let (conversation, author, ts) = key(conversation, message);
            for statement in [
                "DELETE FROM b33p_expiry WHERE conversation = ? AND author = ? AND ts = ?",
                "DELETE FROM b33p_edit WHERE conversation = ? AND author = ? AND ts = ?",
                "DELETE FROM b33p_deleted WHERE conversation = ? AND author = ? AND ts = ?",
            ] {
                sqlx::query(statement)
                    .bind(&conversation)
                    .bind(&author)
                    .bind(ts)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        tx.commit().await
    }
}

/// One conversation's settings, read through `db`: the pool, or a transaction.
async fn settings_in(
    db: impl sqlx::SqliteExecutor<'_>,
    id: &ConversationId,
) -> Result<Option<ChatSettings>, sqlx::Error> {
    let row = sqlx::query(concat!(
        "SELECT ",
        settings!(),
        " FROM b33p_conversation WHERE id = ?"
    ))
    .bind(&id.0)
    .fetch_optional(db)
    .await?;
    Ok(row.as_ref().map(settings_of))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    async fn open(dir: &tempfile::TempDir) -> LocalStore {
        let options = SqliteConnectOptions::new()
            .filename(dir.path().join("local.db3"))
            .create_if_missing(true);
        LocalStore::open(options).await.unwrap()
    }

    /// Changes to one conversation at once all land: each reads the row and writes it back,
    /// and neither may write over what the other changed in between.
    #[test]
    fn concurrent_changes_to_a_conversation_all_land() {
        block_on(async {
            let dir = tempfile::tempdir().unwrap();
            let store = open(&dir).await;
            let id = ConversationId::contact("ann");
            store.update(&id, |_| {}).await.unwrap();
            let (pinned, muted, read) = tokio::join!(
                store.update(&id, |s| s.pinned = Some(0)),
                store.update(&id, |s| s.muted_until = Some(1_000)),
                store.mark_read(&id, 10),
            );
            pinned.unwrap();
            muted.unwrap();
            read.unwrap();
            let settings = store.settings(&id).await.unwrap().unwrap();
            assert_eq!(settings.pinned, Some(0));
            assert_eq!(settings.muted_until, Some(1_000));
            assert_eq!(settings.last_read, 10);
        });
    }

    fn reference(author: &str, ts: u64) -> MessageRef {
        MessageRef {
            author_id: author.into(),
            timestamp: ts,
        }
    }

    /// An older b33p refuses tables a newer one changed in ways it would misread, and keeps
    /// opening its own.
    #[test]
    fn tables_from_a_newer_version_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        block_on(async {
            let local = open(&dir).await;
            local.close().await;
            let local = open(&dir).await;
            sqlx::query("UPDATE b33p_meta SET value = ? WHERE key = 'schema'")
                .bind(SCHEMA_VERSION + 1)
                .execute(&local.db)
                .await
                .unwrap();
            local.close().await;
            let options = SqliteConnectOptions::new().filename(dir.path().join("local.db3"));
            assert!(matches!(
                LocalStore::open(options).await,
                Err(OpenError::TooNew(v)) if v == SCHEMA_VERSION + 1
            ));
        });
    }

    #[test]
    fn remembers_verifications() {
        let dir = tempfile::tempdir().unwrap();
        block_on(async {
            let local = open(&dir).await;
            assert_eq!(local.verification("alice").await.unwrap(), None);
            let verified = |key: u8, verified: bool| Verification {
                identity_key: vec![key; 33],
                state: if verified {
                    State::Verified
                } else {
                    State::Default
                },
            };
            local
                .set_verification("alice", &verified(1, true))
                .await
                .unwrap();
            local
                .set_verification("alice", &verified(2, false))
                .await
                .unwrap();
            assert_eq!(
                local.verification("alice").await.unwrap(),
                Some(verified(2, false)),
                "the latest replaces it"
            );

            let remote = HashMap::from([("alice".to_string(), verified(1, true))]);
            assert!(!local.remote_verifications_read().await.unwrap());
            local.set_remote_verifications(&remote).await.unwrap();
            assert!(local.remote_verifications_read().await.unwrap());
            assert_eq!(local.remote_verifications().await.unwrap(), remote);
            local
                .set_remote_verifications(&HashMap::new())
                .await
                .unwrap();
            assert!(
                local.remote_verifications().await.unwrap().is_empty(),
                "replaced as a whole"
            );
        });
    }

    #[test]
    fn remembers_conversation_state_and_pins() {
        let dir = tempfile::tempdir().unwrap();
        block_on(async {
            let local = open(&dir).await;
            let a = ConversationId("a".into());
            let b = ConversationId("b".into());
            local
                .update(&a, |s| s.muted_until = Some(1_000))
                .await
                .unwrap();
            local.set_pins(&[b.clone(), a.clone()]).await.unwrap();
            let states = local.conversations().await.unwrap();
            assert_eq!(states[&b].pinned, Some(0));
            assert_eq!(states[&a].pinned, Some(1));
            assert!(
                states[&a].muted_until.is_some(),
                "pinning keeps other state"
            );
            local.set_pins(std::slice::from_ref(&a)).await.unwrap();
            let states = local.conversations().await.unwrap();
            assert_eq!((states[&a].pinned, states[&b].pinned), (Some(0), None));

            // Reading and unarchiving touch only their own columns.
            local.update(&b, |s| s.set_archived(true)).await.unwrap();
            let read = local.mark_read(&b, 40).await.unwrap();
            assert_eq!((read.last_read, read.archived), (40, true));
            assert_eq!(local.mark_read(&b, 30).await.unwrap().last_read, 40);
            let back = local.unarchive_unless_muted(&b, 0).await.unwrap();
            assert!(back.is_some_and(|s| !s.archived));
            assert_eq!(local.unarchive_unless_muted(&b, 0).await.unwrap(), None);
            local.update(&a, |s| s.set_archived(true)).await.unwrap();
            assert_eq!(
                local.unarchive_unless_muted(&a, 0).await.unwrap(),
                None,
                "muted"
            );
        });
    }

    #[test]
    fn delivery_never_goes_backwards() {
        let dir = tempfile::tempdir().unwrap();
        block_on(async {
            let local = open(&dir).await;
            let a = ConversationId("a".into());
            local.set_delivery(&a, 5, Delivery::Sent).await.unwrap();
            local.set_delivery(&a, 5, Delivery::Read).await.unwrap();
            local
                .set_delivery(&a, 5, Delivery::Delivered)
                .await
                .unwrap();
            assert_eq!(local.deliveries(&a).await.unwrap()[&5], Delivery::Read);
            assert_eq!(local.conversations_of(&[5, 6]).await.unwrap(), [(a, 5)]);
        });
    }

    #[test]
    fn edits_deletions_and_expiry() {
        let dir = tempfile::tempdir().unwrap();
        block_on(async {
            let local = open(&dir).await;
            let a = ConversationId("a".into());
            let m = reference("alice", 10);
            local.record_edit(&a, &m, 20, "first").await.unwrap();
            local.record_edit(&a, &m, 30, "second").await.unwrap();
            assert_eq!(
                local.edits(&a).await.unwrap()[&m],
                [(20, "first".to_string()), (30, "second".to_string())]
            );
            local.record_deleted(&a, &m).await.unwrap();
            assert_eq!(local.deleted(&a).await.unwrap(), std::slice::from_ref(&m));
            assert!(local.edits(&a).await.unwrap().is_empty());

            local.record_edit(&a, &m, 40, "third").await.unwrap();
            local.set_expiries(&a, &[(m.clone(), 100)]).await.unwrap();
            // The first start counts.
            let bob = reference("bob", 11);
            local
                .set_expiries(&a, &[(m.clone(), 500), (bob, 200)])
                .await
                .unwrap();
            let (due, next) = local.due(150).await.unwrap();
            assert_eq!(due, [(a.clone(), m.clone())]);
            assert_eq!(next, Some(200));
            local.forget(&due).await.unwrap();
            assert!(local.edits(&a).await.unwrap().is_empty());
            assert!(local.deleted(&a).await.unwrap().is_empty());
            assert_eq!(local.due(150).await.unwrap().0, []);
        });
    }

    #[test]
    fn older_databases_gain_new_columns() {
        let dir = tempfile::tempdir().unwrap();
        block_on(async {
            let options = SqliteConnectOptions::new()
                .filename(dir.path().join("local.db3"))
                .create_if_missing(true);
            // As the table was first made.
            let pool = SqlitePoolOptions::new()
                .connect_with(options.clone())
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE b33p_conversation (
                    id TEXT PRIMARY KEY NOT NULL, pinned INTEGER, muted_until INTEGER,
                    archived INTEGER NOT NULL DEFAULT 0, last_read INTEGER NOT NULL DEFAULT 0,
                    marked_unread INTEGER NOT NULL DEFAULT 0,
                    expire_timer INTEGER NOT NULL DEFAULT 0)",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO b33p_conversation (id, last_read) VALUES ('a', 5)")
                .execute(&pool)
                .await
                .unwrap();
            pool.close().await;
            let local = LocalStore::open(options).await.unwrap();
            let a = ConversationId("a".into());
            assert_eq!(local.settings(&a).await.unwrap().unwrap().last_read, 5);
            let blocked = local.update(&a, |s| s.blocked = true).await.unwrap();
            assert!(blocked.blocked && !blocked.accepted);
        });
    }

    #[test]
    fn folders_keep_their_order_and_members() {
        let dir = tempfile::tempdir().unwrap();
        block_on(async {
            let local = open(&dir).await;
            let folders = [
                crate::ChatFolder {
                    name: "Family".into(),
                    included: vec![ConversationId("c:a".into()), ConversationId("g:b".into())],
                    ..Default::default()
                },
                crate::ChatFolder {
                    name: "Groups".into(),
                    all_groups: true,
                    excluded: vec![ConversationId("g:b".into())],
                    ..Default::default()
                },
            ];
            local.set_folders(&folders).await.unwrap();
            assert_eq!(local.folders().await.unwrap(), folders);
            local.set_folders(&folders[1..]).await.unwrap();
            assert_eq!(local.folders().await.unwrap(), folders[1..]);
        });
    }

    #[test]
    fn stories_last_a_day() {
        let dir = tempfile::tempdir().unwrap();
        block_on(async {
            let local = open(&dir).await;
            let story = |ts| StoredStory {
                author: "ann".into(),
                timestamp: ts,
                conversation: ConversationId("c:ann".into()),
                text: "hi".into(),
                ..Default::default()
            };
            local.add_story(&story(10)).await.unwrap();
            local.add_story(&story(20)).await.unwrap();
            local.add_story(&story(20)).await.unwrap(); // once
            local.story_viewed(&reference("ann", 20)).await.unwrap();
            let kept = local.stories(15).await.unwrap();
            assert_eq!(kept.len(), 1);
            assert!(kept[0].viewed && kept[0].pointer.is_none());
        });
    }
}
