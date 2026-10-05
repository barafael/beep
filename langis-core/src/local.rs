//! langis's own data, kept beside presage's in the same encrypted database: what the protocol
//! doesn't carry or presage doesn't keep. Pins, mutes, the archive and read positions per
//! conversation; delivery states, edit history, deletion markers and expiry times per message.
//!
//! It uses its own connection pool, opened with the same options (key, close guard) as the
//! store's, so closing waits for both and nothing is ever written unencrypted.

use std::collections::HashMap;

use sqlx::{
    Row as _, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
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

fn settings_of(row: &sqlx::sqlite::SqliteRow) -> ChatSettings {
    ChatSettings {
        pinned: row.get::<Option<i64>, _>(1).map(|p| p as u32),
        muted_until: row.get::<Option<i64>, _>(2).map(|m| m as u64),
        archived: row.get::<i64, _>(3) != 0,
        last_read: row.get::<i64, _>(4) as u64,
        marked_unread: row.get::<i64, _>(5) != 0,
        expire_timer: row.get::<i64, _>(6) as u32,
    }
}

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS langis_conversation (
        id TEXT PRIMARY KEY NOT NULL,
        pinned INTEGER,
        muted_until INTEGER,
        archived INTEGER NOT NULL DEFAULT 0,
        last_read INTEGER NOT NULL DEFAULT 0,
        marked_unread INTEGER NOT NULL DEFAULT 0,
        expire_timer INTEGER NOT NULL DEFAULT 0
    )",
    "CREATE TABLE IF NOT EXISTS langis_delivery (
        conversation TEXT NOT NULL,
        ts INTEGER NOT NULL,
        state INTEGER NOT NULL,
        PRIMARY KEY (conversation, ts)
    )",
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
        Ok(Self { db })
    }

    /// Conversations langis remembers something about, by id.
    pub(crate) async fn conversations(
        &self,
    ) -> Result<HashMap<ConversationId, ChatSettings>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, pinned, muted_until, archived, last_read, marked_unread, expire_timer FROM langis_conversation",
        )
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
        let row = sqlx::query(
            "SELECT id, pinned, muted_until, archived, last_read, marked_unread, expire_timer FROM langis_conversation WHERE id = ?",
        )
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
                (id, pinned, muted_until, archived, last_read, marked_unread, expire_timer)
             VALUES (?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&id.0)
        .bind(state.pinned.map(i64::from))
        .bind(state.muted_until.map(|m| m.min(i64::MAX as u64) as i64))
        .bind(state.archived as i64)
        .bind(state.last_read as i64)
        .bind(state.marked_unread as i64)
        .bind(i64::from(state.expire_timer))
        .execute(&self.db)
        .await?;
        Ok(state)
    }

    /// Pins exactly `order`, in that order; everything else is unpinned.
    pub(crate) async fn set_pins(&self, order: &[ConversationId]) -> Result<(), sqlx::Error> {
        let mut tx = self.db.begin().await?;
        sqlx::query("UPDATE langis_conversation SET pinned = NULL")
            .execute(&mut *tx)
            .await?;
        for (position, id) in order.iter().enumerate() {
            sqlx::query("INSERT INTO langis_conversation (id, pinned) VALUES (?, ?) ON CONFLICT(id) DO UPDATE SET pinned = excluded.pinned")
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
        sqlx::query(
            "INSERT INTO langis_delivery (conversation, ts, state) VALUES (?, ?, ?)
             ON CONFLICT(conversation, ts) DO UPDATE SET state = max(state, excluded.state)",
        )
        .bind(&conversation.0)
        .bind(ts as i64)
        .bind(delivery as i64)
        .execute(&self.db)
        .await
        .map(drop)
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
            let message = MessageRef {
                author_id: row.get(0),
                timestamp: row.get::<i64, _>(1) as u64,
            };
            edits
                .entry(message)
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
        Ok(rows
            .into_iter()
            .map(|row| MessageRef {
                author_id: row.get(0),
                timestamp: row.get::<i64, _>(1) as u64,
            })
            .collect())
    }

    /// When a disappearing message goes; set once (the first start counts).
    pub(crate) async fn set_expiry(
        &self,
        conversation: &ConversationId,
        message: &MessageRef,
        expires_at: u64,
    ) -> Result<(), sqlx::Error> {
        let (conversation, author, ts) = key(conversation, message);
        sqlx::query("INSERT OR IGNORE INTO langis_expiry (conversation, author, ts, expires_at) VALUES (?, ?, ?, ?)")
            .bind(conversation)
            .bind(author)
            .bind(ts)
            .bind(expires_at as i64)
            .execute(&self.db)
            .await
            .map(drop)
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
            .map(|row| {
                (
                    MessageRef {
                        author_id: row.get(0),
                        timestamp: row.get::<i64, _>(1) as u64,
                    },
                    row.get::<i64, _>(2) as u64,
                )
            })
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
                .map(|row| {
                    (
                        ConversationId(row.get(0)),
                        MessageRef {
                            author_id: row.get(1),
                            timestamp: row.get::<i64, _>(2) as u64,
                        },
                    )
                })
                .collect();
        let next: Option<i64> =
            sqlx::query("SELECT min(expires_at) FROM langis_expiry WHERE expires_at > ?")
                .bind(now as i64)
                .fetch_one(&self.db)
                .await?
                .get(0);
        Ok((due, next.map(|n| n as u64)))
    }

    /// Forgets an expired message; it is gone from the store too.
    pub(crate) async fn forget_expired(
        &self,
        conversation: &ConversationId,
        message: &MessageRef,
    ) -> Result<(), sqlx::Error> {
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
                .execute(&self.db)
                .await?;
        }
        Ok(())
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

            local.set_expiry(&a, &m, 100).await.unwrap();
            local.set_expiry(&a, &m, 500).await.unwrap(); // the first start counts
            local
                .set_expiry(&a, &reference("bob", 11), 200)
                .await
                .unwrap();
            let (due, next) = local.due(150).await.unwrap();
            assert_eq!(due, [(a.clone(), m.clone())]);
            assert_eq!(next, Some(200));
            local.forget_expired(&a, &m).await.unwrap();
            assert!(local.edits(&a).await.unwrap().is_empty());
            assert_eq!(local.due(150).await.unwrap().0, []);
        });
    }
}
