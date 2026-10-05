//! langis's own data, kept beside presage's in the same encrypted database: what the protocol
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

use crate::{ChatSettings, ConversationId, Delivery, MessageRef};

#[derive(Clone)]
pub(crate) struct LocalStore {
    db: SqlitePool,
}

/// A message as identified in langis's tables.
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

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS langis_conversation (
        id TEXT PRIMARY KEY NOT NULL,
        pinned INTEGER,
        muted_until INTEGER,
        archived INTEGER NOT NULL DEFAULT 0,
        last_read INTEGER NOT NULL DEFAULT 0,
        marked_unread INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS langis_delivery (
        conversation TEXT NOT NULL,
        ts INTEGER NOT NULL,
        state INTEGER NOT NULL,
        PRIMARY KEY (conversation, ts)
    )",
    // Receipts name only the time.
    "CREATE INDEX IF NOT EXISTS langis_delivery_ts ON langis_delivery (ts)",
    "CREATE TABLE IF NOT EXISTS langis_edit (
        conversation TEXT NOT NULL,
        author TEXT NOT NULL,
        ts INTEGER NOT NULL,
        edited_at INTEGER NOT NULL,
        previous_body TEXT NOT NULL,
        PRIMARY KEY (conversation, author, ts, edited_at)
    )",
    "CREATE TABLE IF NOT EXISTS langis_deleted (
        conversation TEXT NOT NULL,
        author TEXT NOT NULL,
        ts INTEGER NOT NULL,
        PRIMARY KEY (conversation, author, ts)
    )",
    "CREATE TABLE IF NOT EXISTS langis_expiry (
        conversation TEXT NOT NULL,
        author TEXT NOT NULL,
        ts INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        PRIMARY KEY (conversation, author, ts)
    )",
    "CREATE INDEX IF NOT EXISTS langis_expiry_at ON langis_expiry (expires_at)",
    // What the storage service said last, per conversation; see `storage.rs`. `accepted` is
    // its "whitelisted".
    "CREATE TABLE IF NOT EXISTS langis_remote (
        id TEXT PRIMARY KEY NOT NULL,
        pinned INTEGER,
        muted_until INTEGER,
        archived INTEGER NOT NULL DEFAULT 0,
        marked_unread INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS langis_meta (
        key TEXT PRIMARY KEY NOT NULL,
        value INTEGER NOT NULL
    )",
];

/// Columns added to tables after they first shipped: table, column, and how to add it.
const ADDED_COLUMNS: &[(&str, &str, &str)] = &[
    (
        "langis_conversation",
        "blocked",
        "ALTER TABLE langis_conversation ADD COLUMN blocked INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "langis_conversation",
        "accepted",
        "ALTER TABLE langis_conversation ADD COLUMN accepted INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "langis_remote",
        "blocked",
        "ALTER TABLE langis_remote ADD COLUMN blocked INTEGER NOT NULL DEFAULT 0",
    ),
    (
        "langis_remote",
        "accepted",
        "ALTER TABLE langis_remote ADD COLUMN accepted INTEGER NOT NULL DEFAULT 0",
    ),
];

impl LocalStore {
    pub(crate) async fn open(options: SqliteConnectOptions) -> Result<Self, sqlx::Error> {
        // Little traffic: a couple of connections are plenty.
        let db = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await?;
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
        Ok(Self { db })
    }

    /// Conversations langis remembers something about, by id.
    pub(crate) async fn conversations(
        &self,
    ) -> Result<HashMap<ConversationId, ChatSettings>, sqlx::Error> {
        let rows = sqlx::query(concat!("SELECT ", settings!(), " FROM langis_conversation"))
            .fetch_all(&self.db)
            .await?;
        Ok(rows
            .iter()
            .map(|row| (ConversationId(row.get(0)), settings_of(row)))
            .collect())
    }

    /// What langis remembers about one conversation, if anything.
    pub(crate) async fn settings(
        &self,
        id: &ConversationId,
    ) -> Result<Option<ChatSettings>, sqlx::Error> {
        let row = sqlx::query(concat!(
            "SELECT ",
            settings!(),
            " FROM langis_conversation WHERE id = ?"
        ))
        .bind(&id.0)
        .fetch_optional(&self.db)
        .await?;
        Ok(row.as_ref().map(settings_of))
    }

    /// Changes what langis remembers about one conversation.
    pub(crate) async fn update(
        &self,
        id: &ConversationId,
        change: impl FnOnce(&mut ChatSettings),
    ) -> Result<ChatSettings, sqlx::Error> {
        let mut state = self.settings(id).await?.unwrap_or_default();
        change(&mut state);
        sqlx::query(
            "INSERT OR REPLACE INTO langis_conversation
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
        .execute(&self.db)
        .await?;
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
            "INSERT INTO langis_conversation (id, last_read) VALUES (?, ?)
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
            "UPDATE langis_conversation SET archived = 0
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
        let mut tx = self.db.begin().await?;
        for (id, s) in settings {
            sqlx::query(
                "INSERT INTO langis_conversation
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
             FROM langis_remote",
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
        let mut tx = self.db.begin().await?;
        sqlx::query("DELETE FROM langis_remote")
            .execute(&mut *tx)
            .await?;
        for (id, s) in remote {
            sqlx::query(
                "INSERT INTO langis_remote
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
            "INSERT INTO langis_meta (key, value) VALUES ('storage_version', ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(version.min(i64::MAX as u64) as i64)
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    }

    /// The storage service's manifest version last read; 0 before the first time.
    pub(crate) async fn storage_version(&self) -> Result<u64, sqlx::Error> {
        let version: Option<i64> =
            sqlx::query_scalar("SELECT value FROM langis_meta WHERE key = 'storage_version'")
                .fetch_optional(&self.db)
                .await?;
        Ok(version.unwrap_or(0) as u64)
    }

    /// Pins exactly `order`, in that order; everything else is unpinned.
    pub(crate) async fn set_pins(&self, order: &[ConversationId]) -> Result<(), sqlx::Error> {
        let mut tx = self.db.begin().await?;
        sqlx::query("UPDATE langis_conversation SET pinned = NULL")
            .execute(&mut *tx)
            .await?;
        // As `ChatSettings::set_pinned`: pinned chats aren't archived.
        for (position, id) in order.iter().enumerate() {
            sqlx::query("INSERT INTO langis_conversation (id, pinned) VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET pinned = excluded.pinned, archived = 0")
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
        let mut tx = self.db.begin().await?;
        for (conversation, ts) in messages {
            sqlx::query(
                "INSERT INTO langis_delivery (conversation, ts, state) VALUES (?, ?, ?)
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
        let rows = sqlx::query("SELECT ts, state FROM langis_delivery WHERE conversation = ?")
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
            let rows = sqlx::query("SELECT conversation FROM langis_delivery WHERE ts = ?")
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
        sqlx::query("INSERT OR REPLACE INTO langis_edit (conversation, author, ts, edited_at, previous_body) VALUES (?, ?, ?, ?, ?)")
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
        let rows = sqlx::query("SELECT author, ts, edited_at, previous_body FROM langis_edit WHERE conversation = ? ORDER BY edited_at")
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
        sqlx::query(
            "INSERT OR IGNORE INTO langis_deleted (conversation, author, ts) VALUES (?, ?, ?)",
        )
        .bind(conversation)
        .bind(author)
        .bind(ts)
        .execute(&self.db)
        .await
        .map(drop)
    }

    pub(crate) async fn deleted(
        &self,
        conversation: &ConversationId,
    ) -> Result<Vec<MessageRef>, sqlx::Error> {
        let rows = sqlx::query("SELECT author, ts FROM langis_deleted WHERE conversation = ?")
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
        let mut tx = self.db.begin().await?;
        for (message, expires_at) in expiries {
            let (conversation, author, ts) = key(conversation, message);
            sqlx::query("INSERT OR IGNORE INTO langis_expiry (conversation, author, ts, expires_at) VALUES (?, ?, ?, ?)")
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
            sqlx::query("SELECT author, ts, expires_at FROM langis_expiry WHERE conversation = ?")
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
            sqlx::query("SELECT conversation, author, ts FROM langis_expiry WHERE expires_at <= ?")
                .bind(now as i64)
                .fetch_all(&self.db)
                .await?
                .into_iter()
                .map(|row| (ConversationId(row.get(0)), message_at(&row, 1)))
                .collect();
        let next: Option<i64> =
            sqlx::query("SELECT min(expires_at) FROM langis_expiry WHERE expires_at > ?")
                .bind(now as i64)
                .fetch_one(&self.db)
                .await?
                .get(0);
        Ok((due, next.map(|n| n as u64)))
    }

    /// Forgets expired messages, in one transaction; they are gone from the store too.
    pub(crate) async fn forget_expired(
        &self,
        messages: &[(ConversationId, MessageRef)],
    ) -> Result<(), sqlx::Error> {
        let mut tx = self.db.begin().await?;
        for (conversation, message) in messages {
            let (conversation, author, ts) = key(conversation, message);
            for statement in [
                "DELETE FROM langis_expiry WHERE conversation = ? AND author = ? AND ts = ?",
                "DELETE FROM langis_edit WHERE conversation = ? AND author = ? AND ts = ?",
                "DELETE FROM langis_deleted WHERE conversation = ? AND author = ? AND ts = ?",
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

    fn reference(author: &str, ts: u64) -> MessageRef {
        MessageRef {
            author_id: author.into(),
            timestamp: ts,
        }
    }

    #[test]
    fn remembers_conversation_state_and_pins() {
        let dir = tempfile::tempdir().unwrap();
        block_on(async {
            let local = open(&dir).await;
            let a = ConversationId("a".into());
            let b = ConversationId("b".into());
            local
                .update(&a, |s| s.muted_until = Some(u64::MAX))
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
            local.forget_expired(&due).await.unwrap();
            assert!(local.edits(&a).await.unwrap().is_empty());
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
                "CREATE TABLE langis_conversation (
                    id TEXT PRIMARY KEY NOT NULL, pinned INTEGER, muted_until INTEGER,
                    archived INTEGER NOT NULL DEFAULT 0, last_read INTEGER NOT NULL DEFAULT 0,
                    marked_unread INTEGER NOT NULL DEFAULT 0,
                    expire_timer INTEGER NOT NULL DEFAULT 0)",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query("INSERT INTO langis_conversation (id, last_read) VALUES ('a', 5)")
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
}
