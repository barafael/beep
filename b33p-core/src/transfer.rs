//! Message history transfer from the primary device when linking ("link and sync").
//!
//! The primary device only offers the transfer when the provisioning URL advertises
//! [`LINK_CAPABILITY`]. If the user accepts, the provisioning message carries a one-time
//! 32-byte key, and the phone uploads an encrypted archive in Signal's backup format to the
//! transit CDN. We wait for its location, download it and decrypt it with keys derived from
//! that one-time key and our ACI, then import it (see [`crate::import`]).
//!
//! For now only contacts, groups and text messages are imported.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use futures::AsyncReadExt as _;
use libsignal_account_keys::BackupKey;
use libsignal_message_backup::{
    frame::{FileReaderFactory, FramesReader, VerifyHmac as _},
    key::MessageBackupKey,
    parse::VarintDelimitedReader,
};
use presage::{Error as PresageError, libsignal_service::push_service::ServiceError};
use presage::{
    libsignal_service::{
        configuration::Endpoint,
        protocol::Aci,
        push_service::{HttpAuthOverride, PushService},
    },
    manager::TransferArchive,
    store::ContentsStore,
};
use tracing::{info, warn};

use crate::local::LocalStore;
use crate::{
    Error, Event, Events, HistoryTransfer,
    import::{self, Importer, Stats},
    signal::Mgr,
};

/// Query parameter value that makes the phone offer "Transfer message history".
pub(crate) const LINK_CAPABILITY: &str = "backup5";

/// How long the phone gets to upload the archive, as in Signal Desktop.
const MAX_WAIT: Duration = Duration::from_secs(60 * 60);
/// Ceiling on the downloaded archive. It holds text and pointers to attachments, not the
/// attachments themselves (a long-time user's was well under 1 MB), so this is generous; it only
/// stops a misbehaving server from filling the disk.
const MAX_ARCHIVE_BYTES: u64 = 2_000_000_000;

/// Per request; the HTTP client gives up after 65 seconds.
const POLL: Duration = Duration::from_secs(55);

/// How a transfer ended, short of failing.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Outcome {
    Imported,
    /// The phone's upload failed and the user chose to continue without history.
    WithoutHistory,
    /// The user asked on the phone to link again.
    Relink,
}

/// Waits for the archive the phone uploads after linking, downloads it to `dir` and imports
/// it into the manager's store. On `Err`, part of the history may have been stored.
pub(crate) async fn receive_history(
    manager: &Mgr,
    key: [u8; 32],
    dir: &Path,
    tables: &LocalStore,
    events: &Events,
) -> Result<Outcome, Error> {
    let path = dir.join("history-transfer.bin");
    // However this ends, even if the caller drops it midway (skip, quit), the archive goes.
    let _cleanup = RemoveOnDrop(path.clone());
    async {
        events.send(Event::HistoryTransfer(HistoryTransfer::Waiting));
        let (cdn, cdn_key) = match wait_for_archive(manager).await? {
            TransferArchive::Uploaded { cdn, key } => (cdn, key),
            TransferArchive::ContinueWithoutUpload => return Ok(Outcome::WithoutHistory),
            TransferArchive::RelinkRequested => return Ok(Outcome::Relink),
        };
        events.send(Event::HistoryTransfer(HistoryTransfer::Downloading));
        // A dropped connection shouldn't cost the link (which starts over without the
        // history): a few more tries, from the start.
        let mut attempt = 1;
        loop {
            match download(manager, cdn, &cdn_key, &path).await {
                Ok(()) => break,
                Err(error) if error.retryable() && attempt < DOWNLOAD_ATTEMPTS => {
                    warn!(error = %crate::scrub::scrubbed(&error), attempt, "failed to download the message history; retrying");
                    tokio::time::sleep(Duration::from_secs(5) * attempt).await;
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
        let aci = manager.registration_data().service_ids.aci();
        let mut store = manager.store().clone();
        let stats = import_archive(key, &aci, &path, &mut store, tables, events).await?;
        info!(?stats, "message history imported");
        Ok(Outcome::Imported)
    }
    .await
}

/// The archive is encrypted, but there is no reason to keep it around.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

async fn wait_for_archive(manager: &Mgr) -> Result<TransferArchive, Error> {
    let started = Instant::now();
    info!("waiting for the primary device to upload the message history");
    let mut backoff = Duration::from_secs(1);
    while started.elapsed() < MAX_WAIT {
        match manager.transfer_archive(POLL).await {
            Ok(None) => continue,
            Ok(Some(archive)) => {
                info!(
                    waited_secs = started.elapsed().as_secs(),
                    "the primary device answered"
                );
                return Ok(archive);
            }
            // The link itself is no good; waiting longer won't help.
            Err(PresageError::ServiceError(ServiceError::Unauthorized)) => {
                return Err(Error::refused("Signal no longer accepts this device."));
            }
            // A network hiccup shouldn't throw away the link: the archive may still come.
            Err(error) => {
                warn!(error = %crate::scrub::scrubbed(&error), "failed to ask for the message history; retrying");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(30));
            }
        }
    }
    Err(Error::unreachable(
        "The phone did not send the message history within an hour.",
    ))
}

/// How often the archive is downloaded before giving up.
const DOWNLOAD_ATTEMPTS: u32 = 3;

/// Streams the archive to `path`; it can be hundreds of megabytes.
/// Fails [`ErrorKind::Unreachable`](crate::ErrorKind::Unreachable) when trying again may work.
async fn download(manager: &Mgr, cdn: u32, cdn_key: &str, path: &Path) -> Result<(), Error> {
    let too_big = || {
        format!(
            "The message history is larger than {} MB, which no phone sends.",
            MAX_ARCHIVE_BYTES / 1_000_000
        )
    };

    let servers = manager.registration_data().signal_servers.clone();
    let mut response = PushService::new(servers, None, "b33p")
        .request(
            reqwest::Method::GET,
            Endpoint::cdn(cdn, format!("/attachments/{cdn_key}")),
            HttpAuthOverride::Unidentified,
        )
        .map_err(|e| Error::refused(format!("invalid archive location: {e}")))?
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(crate::signal::errors::http(
            "failed to download the message history",
        ))?;
    // Refused before writing anything when the server says, and while writing otherwise, so a
    // misbehaving server can't fill the disk.
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ARCHIVE_BYTES)
    {
        return Err(Error::refused(too_big()));
    }
    // Private to the user and complete or absent: built under a temporary name.
    let mut file = crate::vault::write_private_file(path)
        .map_err(|e| Error::storage(format!("failed to create {}: {e}", path.display())))?;
    let mut size = 0u64;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| Error::unreachable(format!("failed to download the message history: {e}")))?
    {
        size += chunk.len() as u64;
        if size > MAX_ARCHIVE_BYTES {
            return Err(Error::refused(too_big()));
        }
        file.write_all(&chunk)
            .map_err(|e| Error::storage(format!("failed to write {}: {e}", path.display())))?;
    }
    file.sync_all()
        .map_err(|e| Error::storage(format!("failed to write {}: {e}", path.display())))?;
    file.persist()
        .map_err(|e| Error::storage(format!("failed to write {}: {e}", path.display())))?;
    info!(bytes = size, "history archive downloaded");
    Ok(())
}

/// Decrypts the archive and imports it frame by frame. `FramesReader` checks the HMAC over the
/// whole file before handing out any frame, so nothing unauthenticated is stored.
async fn import_archive<S: ContentsStore>(
    key: [u8; 32],
    aci: &Aci,
    path: &Path,
    store: &mut S,
    tables: &LocalStore,
    events: &Events,
) -> Result<Stats, Error>
where
    S::ContentsStoreError: std::fmt::Display,
{
    let key = message_backup_key(key, aci);
    let frames = FramesReader::new(&key, FileReaderFactory { path })
        .await
        .map_err(|e| Error::refused(format!("failed to decrypt the message history: {e}")))?;
    let mut reader = VarintDelimitedReader::new(frames);
    let mut importer = Importer::new(*aci, crate::now_millis());
    // The first frame is a `BackupInfo` (version and time), which we don't need.
    let mut info_frame = true;
    let mut reported = 0;
    while let Some(frame) = reader
        .read_next()
        .await
        .map_err(|e| Error::refused(format!("failed to read the message history: {e}")))?
    {
        if std::mem::take(&mut info_frame) {
            continue;
        }
        for record in importer.frame(&frame).map_err(Error::refused)? {
            import::store(store, tables, record).await?;
        }
        let messages = importer.stats().messages;
        if messages >= reported + 500 {
            reported = messages;
            events.send(Event::HistoryTransfer(HistoryTransfer::Importing {
                messages,
            }));
        }
    }
    for record in importer.finish() {
        import::store(store, tables, record).await?;
    }
    // Drain any trailing bytes so the HMAC covers everything, then check it once more.
    let mut rest = reader.into_inner();
    let mut sink = Vec::new();
    rest.read_to_end(&mut sink)
        .await
        .map_err(|e| Error::refused(format!("failed to read the message history: {e}")))?;
    rest.verify_hmac().await.map_err(|e| {
        Error::refused(format!(
            "the message history failed its integrity check: {e}"
        ))
    })?;
    Ok(importer.stats())
}

/// The one-time key is used directly as the backup key, with no forward secrecy token.
fn message_backup_key(key: [u8; 32], aci: &Aci) -> MessageBackupKey {
    let backup_key: BackupKey = BackupKey(key);
    MessageBackupKey::derive(&backup_key, &backup_key.derive_backup_id(aci), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libsignal_message_backup::{BackupReader, backup::Purpose};
    use presage::model::identity::OnNewIdentity;
    use presage_store_sqlite::SqliteStore;

    /// libsignal's `legacy-account.binproto.encrypted` test case: the same format as a
    /// link-and-sync archive (no forward secrecy token), with a known key. It holds account
    /// data, our own recipient and a chat with it.
    const ARCHIVE: &[u8] = include_bytes!("testdata/legacy-account.binproto.encrypted");

    fn fixture_key() -> [u8; 32] {
        let pool = "m".repeat(64).parse().expect("valid account entropy pool");
        BackupKey::derive_from_account_entropy_pool(&pool).0
    }

    fn fixture_aci() -> Aci {
        Aci::from_uuid_bytes([0x11; 16])
    }

    /// A scratch file and an empty store, both removed on drop.
    struct Scratch {
        dir: PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("b33p-transfer-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("archive"), ARCHIVE).unwrap();
            Self { dir }
        }

        fn archive(&self) -> PathBuf {
            self.dir.join("archive")
        }

        async fn store(&self) -> SqliteStore {
            let url = format!("sqlite://{}?mode=rwc", self.dir.join("store.db3").display());
            SqliteStore::open(&url, OnNewIdentity::Trust).await.unwrap()
        }

        async fn tables(&self) -> LocalStore {
            let options = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(self.dir.join("local.db3"))
                .create_if_missing(true);
            LocalStore::open(options).await.unwrap()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Progress events go nowhere in tests.
    fn events() -> Events {
        Events::new(async_channel::unbounded().0)
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    #[test]
    fn imports_a_legacy_archive() {
        let scratch = Scratch::new("import");
        let stats = block_on(async {
            let mut store = scratch.store().await;
            import_archive(
                fixture_key(),
                &fixture_aci(),
                &scratch.archive(),
                &mut store,
                &scratch.tables().await,
                &events(),
            )
            .await
        });
        // Only the account frame is not imported; there are no contacts or messages.
        assert_eq!(
            stats.unwrap(),
            Stats {
                skipped: 1,
                ..Stats::default()
            }
        );
    }

    #[test]
    fn rejects_the_wrong_account() {
        let scratch = Scratch::new("wrong-account");
        let result = block_on(async {
            let mut store = scratch.store().await;
            import_archive(
                fixture_key(),
                &Aci::from_uuid_bytes([0x22; 16]),
                &scratch.archive(),
                &mut store,
                &scratch.tables().await,
                &events(),
            )
            .await
        });
        assert!(result.is_err());
    }

    /// Our key derivation matches what libsignal's own validator accepts.
    #[test]
    fn libsignal_validates_the_archive_with_our_key() {
        let scratch = Scratch::new("validate");
        let key = message_backup_key(fixture_key(), &fixture_aci());
        let result = block_on(async {
            BackupReader::new_encrypted_compressed(
                &key,
                FileReaderFactory {
                    path: scratch.archive(),
                },
                Purpose::DeviceTransfer,
            )
            .await
            .unwrap()
            .validate_all()
            .await
            .result
        });
        assert!(result.is_ok(), "{result:?}");
    }
}
