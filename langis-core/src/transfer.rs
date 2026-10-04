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
use presage::{
    libsignal_service::{
        configuration::Endpoint,
        protocol::Aci,
        push_service::{HttpAuthOverride, PushService},
    },
    manager::TransferArchive,
    store::ContentsStore,
};
use tracing::info;

use crate::{
    Event, Events, HistoryTransfer,
    import::{self, Importer, Stats},
    signal::Mgr,
};

/// Query parameter value that makes the phone offer "Transfer message history".
pub(crate) const LINK_CAPABILITY: &str = "backup5";

/// How long the phone gets to upload the archive, as in Signal Desktop.
const MAX_WAIT: Duration = Duration::from_secs(60 * 60);
/// Per request; the HTTP client gives up after 65 seconds.
const POLL: Duration = Duration::from_secs(55);

/// Waits for the archive the phone uploads after linking, downloads it to `dir` and imports
/// it into the manager's store. Returns `Ok(())` without doing anything if the user chose to
/// continue without history.
pub(crate) async fn receive_history(manager: &Mgr, key: [u8; 32], dir: &Path, events: &Events) -> Result<(), String> {
    let path = dir.join("history-transfer.bin");
    let result = async {
        let _ = events.send(Event::HistoryTransfer(HistoryTransfer::Waiting));
        let Some((cdn, cdn_key)) = wait_for_archive(manager).await? else {
            return Ok(());
        };
        let _ = events.send(Event::HistoryTransfer(HistoryTransfer::Downloading));
        download(manager, cdn, &cdn_key, &path).await?;
        let aci = manager.registration_data().service_ids.aci();
        let stats = import_archive(key, &aci, &path, &mut manager.store().clone(), events).await?;
        info!(?stats, "message history imported");
        Ok(())
    }
    .await;
    let _ = events.send(Event::HistoryTransfer(HistoryTransfer::Finished));
    // The archive is encrypted, but there is no reason to keep it around.
    let _ = std::fs::remove_file(&path);
    result
}

async fn wait_for_archive(manager: &Mgr) -> Result<Option<(u32, String)>, String> {
    let started = Instant::now();
    info!("waiting for the primary device to upload the message history");
    while started.elapsed() < MAX_WAIT {
        match manager.transfer_archive(POLL).await {
            Ok(None) => continue,
            Ok(Some(TransferArchive::Uploaded { cdn, key })) => {
                info!(cdn, waited_secs = started.elapsed().as_secs(), "history archive uploaded");
                return Ok(Some((cdn, key)));
            }
            Ok(Some(TransferArchive::ContinueWithoutUpload)) => {
                info!("the primary device continues without transferring history");
                return Ok(None);
            }
            Ok(Some(TransferArchive::RelinkRequested)) => {
                return Err("The phone asked to link this device again.".into());
            }
            Err(error) => return Err(format!("failed to wait for the message history: {error}")),
        }
    }
    Err("The phone did not send the message history within an hour.".into())
}

/// Streams the archive to `path`; it can be hundreds of megabytes.
async fn download(manager: &Mgr, cdn: u32, cdn_key: &str, path: &PathBuf) -> Result<(), String> {
    use std::io::Write as _;

    let servers = manager.registration_data().signal_servers;
    let mut response = PushService::new(servers, None, "langis")
        .request(
            reqwest::Method::GET,
            Endpoint::cdn(cdn, format!("/attachments/{cdn_key}")),
            HttpAuthOverride::Unidentified,
        )
        .map_err(|e| format!("invalid archive location: {e}"))?
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("failed to download the message history: {e}"))?;
    let mut file = std::fs::File::create(path).map_err(|e| format!("failed to create {}: {e}", path.display()))?;
    let mut size = 0usize;
    while let Some(chunk) =
        response.chunk().await.map_err(|e| format!("failed to download the message history: {e}"))?
    {
        size += chunk.len();
        file.write_all(&chunk).map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    }
    file.sync_all().map_err(|e| format!("failed to write {}: {e}", path.display()))?;
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
    events: &Events,
) -> Result<Stats, String>
where
    S::ContentsStoreError: std::fmt::Display,
{
    let key = message_backup_key(key, aci);
    let frames = FramesReader::new(&key, FileReaderFactory { path })
        .await
        .map_err(|e| format!("failed to decrypt the message history: {e}"))?;
    let mut reader = VarintDelimitedReader::new(frames);
    let mut importer = Importer::new(*aci);
    // The first frame is a `BackupInfo` (version and time), which we don't need.
    let mut info_frame = true;
    while let Some(frame) = reader.read_next().await.map_err(|e| format!("failed to read the message history: {e}"))? {
        if std::mem::take(&mut info_frame) {
            continue;
        }
        for record in importer.frame(&frame)? {
            import::store(store, record).await?;
        }
        let messages = importer.stats().messages;
        if messages > 0 && messages % 500 == 0 {
            let _ = events.send(Event::HistoryTransfer(HistoryTransfer::Importing { messages }));
        }
    }
    for record in importer.finish() {
        import::store(store, record).await?;
    }
    // Drain any trailing bytes so the HMAC covers everything, then check it once more.
    let mut rest = reader.into_inner();
    let mut sink = Vec::new();
    rest.read_to_end(&mut sink).await.map_err(|e| format!("failed to read the message history: {e}"))?;
    rest.verify_hmac().await.map_err(|e| format!("the message history failed its integrity check: {e}"))?;
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
            let dir = std::env::temp_dir().join(format!("langis-transfer-{name}-{}", std::process::id()));
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
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Progress events go nowhere in tests.
    fn events() -> Events {
        Events(async_channel::unbounded().0)
    }

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(future)
    }

    #[test]
    fn imports_a_legacy_archive() {
        let scratch = Scratch::new("import");
        let stats = block_on(async {
            let mut store = scratch.store().await;
            import_archive(fixture_key(), &fixture_aci(), &scratch.archive(), &mut store, &events()).await
        });
        // Only the account frame is not imported; there are no contacts or messages.
        assert_eq!(stats.unwrap(), Stats { skipped: 1, ..Stats::default() });
    }

    #[test]
    fn rejects_the_wrong_account() {
        let scratch = Scratch::new("wrong-account");
        let result = block_on(async {
            let mut store = scratch.store().await;
            import_archive(fixture_key(), &Aci::from_uuid_bytes([0x22; 16]), &scratch.archive(), &mut store, &events()).await
        });
        assert!(result.is_err());
    }

    /// Our key derivation matches what libsignal's own validator accepts.
    #[test]
    fn libsignal_validates_the_archive_with_our_key() {
        let scratch = Scratch::new("validate");
        let key = message_backup_key(fixture_key(), &fixture_aci());
        let result = block_on(async {
            BackupReader::new_encrypted_compressed(&key, FileReaderFactory { path: scratch.archive() }, Purpose::DeviceTransfer)
                .await
                .unwrap()
                .validate_all()
                .await
                .result
        });
        assert!(result.is_ok(), "{result:?}");
    }
}
