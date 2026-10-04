//! Message history transfer from the primary device when linking ("link and sync").
//!
//! The primary device only offers the transfer when the provisioning URL advertises
//! [`LINK_CAPABILITY`]. If the user accepts, the provisioning message carries a one-time
//! 32-byte key, and the phone uploads an encrypted archive in Signal's backup format to the
//! transit CDN. We wait for its location, download it and decrypt it with keys derived from
//! that one-time key and our ACI.
//!
//! PHASE 0 SPIKE: this only verifies the pipeline and logs frame counts; nothing is imported.

use std::{
    collections::BTreeMap,
    path::Path,
    time::{Duration, Instant},
};

use futures::AsyncReadExt as _;
use libsignal_account_keys::BackupKey;
use libsignal_message_backup::{
    BackupReader,
    backup::Purpose,
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
};
use tracing::{info, warn};

use crate::signal::Mgr;

/// Query parameter value that makes the phone offer "Transfer message history".
pub(crate) const LINK_CAPABILITY: &str = "backup5";

/// How long the phone gets to upload the archive, as in Signal Desktop.
const MAX_WAIT: Duration = Duration::from_secs(60 * 60);
/// Per request; the HTTP client gives up after 65 seconds.
const POLL: Duration = Duration::from_secs(55);

/// Ceiling on the downloaded archive. Signal's own clients cap a device transfer at 500 MB;
/// going over that means something other than a phone answered, and an unbounded download
/// would let a hostile server fill the disk. The real size is checked against the length the
/// phone reports, so a legitimately large backup still works.
const MAX_ARCHIVE_BYTES: u64 = 500 * 1024 * 1024;

/// Waits for the archive the phone uploads after linking, downloads it to `dir` and
/// decrypts it. Returns `Ok(())` without doing anything if the user chose to continue
/// without history.
pub(crate) async fn receive_history(manager: &Mgr, key: [u8; 32], dir: &Path) -> Result<(), String> {
    let Some((cdn, cdn_key)) = wait_for_archive(manager).await? else {
        return Ok(());
    };
    let path = dir.join("history-transfer.bin");
    let result = async {
        download(manager, cdn, &cdn_key, &path).await?;
        let aci = manager.registration_data().service_ids.aci();
        let counts = summarize(key, &aci, &path).await?;
        info!(?counts, "history archive decrypted");
        Ok(())
    }
    .await;
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
async fn download(manager: &Mgr, cdn: u32, cdn_key: &str, path: &Path) -> Result<(), String> {
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
    // The length, when the CDN sends one, is checked before a single byte is written rather
    // than discovered after the disk is already full.
    if let Some(length) = response.content_length()
        && length > MAX_ARCHIVE_BYTES
    {
        return Err(format!(
            "the message history is {length} bytes, which is over the {} MiB limit",
            MAX_ARCHIVE_BYTES / 1024 / 1024
        ));
    }
    // 0600: the archive is encrypted, but it is still the user's own history in transit, and the
    // decrypted messages are about to be derived from it.
    let mut file = crate::vault::write_private_file(path)
        .map_err(|e| format!("failed to create {}: {e}", path.display()))?;
    let mut size = 0u64;
    while let Some(chunk) =
        response.chunk().await.map_err(|e| format!("failed to download the message history: {e}"))?
    {
        size += chunk.len() as u64;
        if size > MAX_ARCHIVE_BYTES {
            return Err(format!(
                "the message history is larger than the {} MiB limit",
                MAX_ARCHIVE_BYTES / 1024 / 1024
            ));
        }
        file.write_all(&chunk).map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    }
    file.sync_all().map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    // Moves the temporary into place; until this the file is still incomplete, and dropping
    // instead would leave the user with no archive at all.
    file.persist().map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    info!(bytes = size, "history archive downloaded");
    Ok(())
}

/// Decrypts the archive and counts its frames by kind, then lets libsignal validate it.
async fn summarize(key: [u8; 32], aci: &Aci, path: &Path) -> Result<BTreeMap<&'static str, usize>, String> {
    // The one-time key is used directly as the backup key; there is no forward secrecy token.
    let backup_key: BackupKey = BackupKey(key);
    let backup_id = backup_key.derive_backup_id(aci);
    let key = MessageBackupKey::derive(&backup_key, &backup_id, None);

    let frames = FramesReader::new(&key, FileReaderFactory { path })
        .await
        .map_err(|e| format!("failed to decrypt the message history: {e}"))?;
    let mut reader = VarintDelimitedReader::new(frames);
    // The first frame is a `BackupInfo`; every later one is a `Frame`, whose only field is a
    // oneof, so its first tag tells the kind.
    let mut info_frame = true;
    let mut counts = BTreeMap::<&str, usize>::new();
    while let Some(frame) = reader.read_next().await.map_err(|e| format!("failed to read the message history: {e}"))? {
        if std::mem::take(&mut info_frame) {
            continue;
        }
        *counts.entry(frame_kind(&frame)).or_default() += 1;
    }
    // Drain any trailing bytes so the HMAC covers everything, then check it.
    let mut rest = reader.into_inner();
    let mut sink = Vec::new();
    rest.read_to_end(&mut sink).await.map_err(|e| format!("failed to read the message history: {e}"))?;
    rest.verify_hmac().await.map_err(|e| format!("the message history failed its integrity check: {e}"))?;

    let validation = BackupReader::new_encrypted_compressed(&key, FileReaderFactory { path }, Purpose::DeviceTransfer)
        .await
        .map_err(|e| format!("failed to decrypt the message history: {e}"))?
        .validate_all()
        .await;
    match validation.result {
        Ok(()) => info!(unknown_fields = validation.found_unknown_fields.len(), "history archive is valid"),
        Err(error) => warn!(%error, "libsignal rejected the history archive"),
    }
    Ok(counts)
}

/// Names the oneof field of a `Frame` (see libsignal's `backup.proto`).
fn frame_kind(frame: &[u8]) -> &'static str {
    match frame.first().map(|tag| tag >> 3) {
        Some(1) => "account",
        Some(2) => "recipient",
        Some(3) => "chat",
        Some(4) => "chat_item",
        Some(5) => "sticker_pack",
        Some(6) => "ad_hoc_call",
        Some(7) => "notification_profile",
        Some(8) => "chat_folder",
        None => "empty",
        Some(_) => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// libsignal's `legacy-account.binproto.encrypted` test case: the same format as a
    /// link-and-sync archive (no forward secrecy token), with a known key.
    const ARCHIVE: &[u8] = include_bytes!("testdata/legacy-account.binproto.encrypted");

    fn fixture_key() -> [u8; 32] {
        let pool = "m".repeat(64).parse().expect("valid account entropy pool");
        BackupKey::derive_from_account_entropy_pool(&pool).0
    }

    /// Writes the fixture into a private temporary directory that cleans itself up.
    fn archive_file(dir: &tempfile::TempDir) -> std::path::PathBuf {
        let path = dir.path().join("archive.bin");
        std::fs::write(&path, ARCHIVE).unwrap();
        path
    }

    #[test]
    fn decrypts_a_legacy_archive() {
        let dir = tempfile::tempdir().unwrap();
        let path = archive_file(&dir);
        let aci = Aci::from_uuid_bytes([0x11; 16]);
        let counts = futures::executor::block_on(summarize(fixture_key(), &aci, &path));
        assert_eq!(counts.unwrap(), BTreeMap::from([("account", 1), ("chat", 1), ("recipient", 1)]));
    }

    #[test]
    fn rejects_the_wrong_account() {
        let dir = tempfile::tempdir().unwrap();
        let path = archive_file(&dir);
        let aci = Aci::from_uuid_bytes([0x22; 16]);
        let counts = futures::executor::block_on(summarize(fixture_key(), &aci, &path));
        assert!(counts.is_err());
    }

    #[test]
    fn the_size_limit_is_far_above_a_real_backup() {
        // The limit must not get in the way of a legitimate large backup.
        assert!(MAX_ARCHIVE_BYTES >= 100 * 1024 * 1024);
    }
}
