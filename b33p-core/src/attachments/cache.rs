//! Attachments kept on disk, encrypted, so that a picture is downloaded once rather than each
//! time it shows: the decrypted original (for the viewer, saving and opening) and the picture
//! as the chat shows it (decoded far faster than the original).
//!
//! Under `<data dir>/attachments/`, a file per attachment and kind, named by an HMAC of the
//! attachment's id (its CDN location is nobody else's business). Each file is AES-256-GCM in
//! chunks of [`CHUNK`]: a header of [`MAGIC`] and a random nonce base, then the chunks, each
//! sealed with the base XOR its index as nonce and, as associated data, the file's name, its
//! index and whether it is the last. A file cut short, its chunks reordered, or one put in
//! another's place doesn't open. Both keys come from the database's by HKDF, so the files are
//! as private as the messages, and are gone with the key ([`clear`]).
//!
//! Blocking: callers run these on a blocking thread.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use aes_gcm::aead::{Aead as _, Payload};
use aes_gcm::{Aes256Gcm, KeyInit as _};
use hmac::{Hmac, Mac as _};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::AttachmentId;
use crate::vault::{self, PrivateFileError};

/// The directory under the data directory.
pub(crate) const DIR: &str = "attachments";
/// What a cache file starts with: the format, should it ever change.
const MAGIC: &[u8; 8] = b"b33pc\x00\x00\x01";
const NONCE: usize = 12;
const TAG: usize = 16;
/// Plaintext per chunk.
const CHUNK: usize = 64 * 1024;
/// HKDF info for the cipher's key and the names' key.
const CIPHER_INFO: &[u8] = b"b33p attachment cache v1: cipher";
const NAMES_INFO: &[u8] = b"b33p attachment cache v1: names";

/// What of an attachment a file holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// The attachment as sent, decrypted.
    Original,
    /// The picture as the chat shows it ([`super::PICTURE_EDGE`] and the like), as PNG.
    Shown,
}

impl Entry {
    fn suffix(self) -> &'static str {
        match self {
            Self::Original => "orig",
            Self::Shown => "shown",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum CacheError {
    #[error("making the attachment cache's directory")]
    Dir(#[source] PrivateFileError),
    #[error("writing {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: PrivateFileError,
    },
}

/// The cache of one account's attachments: where, and its keys.
pub(crate) struct Cache {
    dir: PathBuf,
    cipher: Aes256Gcm,
    names: Zeroizing<[u8; 32]>,
}

impl std::fmt::Debug for Cache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cache")
            .field("dir", &self.dir)
            .finish_non_exhaustive()
    }
}

impl Cache {
    /// The cache in `data_dir`, keyed from the database's `key`; its directory made (and kept
    /// to its owner) if missing.
    pub(crate) fn open(data_dir: &Path, key: &vault::Key) -> Result<Self, CacheError> {
        let dir = data_dir.join(DIR);
        vault::restrict_dir(&dir).map_err(CacheError::Dir)?;
        Ok(Self::with_dir(dir, key))
    }

    fn with_dir(dir: PathBuf, key: &vault::Key) -> Self {
        let hkdf = hkdf::Hkdf::<Sha256>::new(None, &key[..]);
        let mut cipher = Zeroizing::new([0u8; 32]);
        let mut names = Zeroizing::new([0u8; 32]);
        hkdf.expand(CIPHER_INFO, &mut cipher[..])
            .expect("32 bytes is a valid length");
        hkdf.expand(NAMES_INFO, &mut names[..])
            .expect("32 bytes is a valid length");
        Self {
            dir,
            cipher: Aes256Gcm::new((&*cipher).into()),
            names,
        }
    }

    /// The file's name: the HMAC of the id, and what it holds.
    fn name(&self, id: &AttachmentId, entry: Entry) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.names[..]).expect("any key length");
        mac.update(id.0.as_bytes());
        format!(
            "{}.{}",
            hex::encode(mac.finalize().into_bytes()),
            entry.suffix()
        )
    }

    /// What is kept of `id` as `entry`; `None` if nothing is, or what is doesn't open (a file
    /// cut short by a crash, or damaged), which is then removed.
    pub(crate) fn read(&self, id: &AttachmentId, entry: Entry) -> Option<Vec<u8>> {
        let name = self.name(id, entry);
        let path = self.dir.join(&name);
        let sealed = match fs::read(&path) {
            Ok(sealed) => sealed,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
            Err(error) => {
                tracing::warn!(error = %error, "could not read a cached attachment");
                return None;
            }
        };
        let opened = self.unseal(&name, &sealed);
        if opened.is_none() {
            tracing::warn!(?entry, "a cached attachment didn't open; removing it");
            remove(&path);
        } else {
            touch(&path);
        }
        opened
    }

    /// Keeps `data` as `entry` of `id`, in place of anything kept before.
    pub(crate) fn write(
        &self,
        id: &AttachmentId,
        entry: Entry,
        data: &[u8],
    ) -> Result<(), CacheError> {
        let name = self.name(id, entry);
        let path = self.dir.join(&name);
        let failed = |source| CacheError::Write {
            path: path.clone(),
            source,
        };
        let mut file = vault::write_private_file(&path).map_err(failed)?;
        file.write_all(&self.seal(&name, data)).map_err(failed)?;
        // Not synced: a cache file lost to a power cut is downloaded again, one cut short
        // doesn't open and is removed.
        file.persist().map_err(failed)
    }

    /// Removes what is kept of `id`.
    pub(crate) fn forget(&self, id: &AttachmentId) {
        for entry in [Entry::Original, Entry::Shown] {
            remove(&self.dir.join(self.name(id, entry)));
        }
    }

    /// Removes the originals used longest ago until they take at most `limit` bytes. What the
    /// chat shows stays: it is small, and what a chat's history scrolls through.
    pub(crate) fn trim(&self, limit: u64) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        let mut originals: Vec<(SystemTime, u64, PathBuf)> = entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.ends_with(Entry::Original.suffix()))
            })
            .filter_map(|entry| {
                let metadata = entry.metadata().ok()?;
                Some((metadata.modified().ok()?, metadata.len(), entry.path()))
            })
            .collect();
        let mut total: u64 = originals.iter().map(|(_, len, _)| len).sum();
        originals.sort();
        for (_, len, path) in originals {
            if total <= limit {
                break;
            }
            remove(&path);
            total = total.saturating_sub(len);
        }
    }

    fn seal(&self, name: &str, plaintext: &[u8]) -> Vec<u8> {
        let base: [u8; NONCE] = rand::random();
        let chunks = plaintext.len().div_ceil(CHUNK).max(1);
        let mut out = Vec::with_capacity(MAGIC.len() + NONCE + plaintext.len() + chunks * TAG);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&base);
        for index in 0..chunks {
            let chunk = &plaintext
                [(index * CHUNK).min(plaintext.len())..((index + 1) * CHUNK).min(plaintext.len())];
            let last = index + 1 == chunks;
            let aad = associated(name, index as u64, last);
            let sealed = self
                .cipher
                .encrypt(
                    (&nonce(&base, index as u64)).into(),
                    Payload {
                        msg: chunk,
                        aad: &aad,
                    },
                )
                .expect("AES-GCM encryption doesn't fail");
            out.extend_from_slice(&sealed);
        }
        out
    }

    fn unseal(&self, name: &str, sealed: &[u8]) -> Option<Vec<u8>> {
        let rest = sealed.strip_prefix(MAGIC.as_slice())?;
        let (base, mut rest) = rest.split_first_chunk::<NONCE>()?;
        let mut out = Vec::with_capacity(rest.len());
        let mut index: u64 = 0;
        loop {
            let take = rest.len().min(CHUNK + TAG);
            let (chunk, after) = rest.split_at(take);
            let last = after.is_empty();
            let aad = associated(name, index, last);
            let opened = self
                .cipher
                .decrypt(
                    (&nonce(base, index)).into(),
                    Payload {
                        msg: chunk,
                        aad: &aad,
                    },
                )
                .ok()?;
            out.extend_from_slice(&opened);
            if last {
                return Some(out);
            }
            rest = after;
            index += 1;
        }
    }
}

/// Removes the cache in `data_dir`, as when the account goes.
pub(crate) fn clear(data_dir: &Path) -> io::Result<()> {
    match fs::remove_dir_all(data_dir.join(DIR)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Removes what the cache in `data_dir` keeps but leaves it there, as when another account is
/// linked.
pub(crate) fn empty(data_dir: &Path) -> io::Result<()> {
    clear(data_dir)?;
    vault::restrict_dir(&data_dir.join(DIR)).map_err(io::Error::other)
}

fn nonce(base: &[u8; NONCE], index: u64) -> [u8; NONCE] {
    let mut nonce = *base;
    for (byte, counter) in nonce[NONCE - 8..].iter_mut().zip(index.to_be_bytes()) {
        *byte ^= counter;
    }
    nonce
}

fn associated(name: &str, index: u64, last: bool) -> Vec<u8> {
    [name.as_bytes(), &index.to_be_bytes(), &[u8::from(last)]].concat()
}

fn remove(path: &Path) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(error = %error, "could not remove a cached attachment");
    }
}

/// Marks a file as used now, for [`Cache::trim`]'s order.
fn touch(path: &Path) {
    let touched = fs::File::options()
        .append(true)
        .open(path)
        .and_then(|file| file.set_modified(SystemTime::now()));
    if let Err(error) = touched {
        tracing::debug!(error = %error, "could not mark a cached attachment as used");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(dir: &Path, byte: u8) -> Cache {
        Cache::with_dir(dir.to_owned(), &Zeroizing::new([byte; 32]))
    }

    fn id(s: &str) -> AttachmentId {
        AttachmentId(s.to_owned())
    }

    #[test]
    fn keeps_what_it_is_given_in_every_size() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path(), 1);
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK + 17] {
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let at = id(&format!("3/{len}"));
            cache.write(&at, Entry::Original, &data).unwrap();
            assert_eq!(cache.read(&at, Entry::Original).as_deref(), Some(&data[..]));
        }
        assert_eq!(cache.read(&id("3/missing"), Entry::Original), None);
    }

    #[test]
    fn names_hide_the_id_and_kinds_stay_apart() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path(), 1);
        let at = id("3/aSecretCdnKey");
        cache.write(&at, Entry::Original, b"original").unwrap();
        cache.write(&at, Entry::Shown, b"shown").unwrap();
        for entry in fs::read_dir(dir.path()).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            assert!(!name.contains("aSecretCdnKey"), "{name}");
        }
        assert_eq!(
            cache.read(&at, Entry::Shown).as_deref(),
            Some(&b"shown"[..])
        );
        let contents = fs::read(dir.path().join(cache.name(&at, Entry::Original))).unwrap();
        assert!(!contents.windows(8).any(|w| w == b"original"));
    }

    #[test]
    fn what_was_tampered_with_cut_short_swapped_or_keyed_otherwise_doesnt_open() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path(), 1);
        let data = vec![7u8; 2 * CHUNK + 5];
        let (a, b) = (id("3/a"), id("3/b"));
        let path = |at: &AttachmentId| dir.path().join(cache.name(at, Entry::Original));
        let reset = || {
            cache.write(&a, Entry::Original, &data).unwrap();
            cache.write(&b, Entry::Original, b"b").unwrap();
        };

        reset();
        let mut sealed = fs::read(path(&a)).unwrap();
        sealed[MAGIC.len() + NONCE + 100] ^= 1;
        fs::write(path(&a), &sealed).unwrap();
        assert_eq!(cache.read(&a, Entry::Original), None);
        assert!(!path(&a).exists(), "what didn't open is removed");

        // Cut at a chunk's end: the chunk before isn't marked last.
        reset();
        let sealed = fs::read(path(&a)).unwrap();
        fs::write(path(&a), &sealed[..MAGIC.len() + NONCE + CHUNK + TAG]).unwrap();
        assert_eq!(cache.read(&a, Entry::Original), None);

        reset();
        fs::copy(path(&b), path(&a)).unwrap();
        assert_eq!(cache.read(&a, Entry::Original), None);

        reset();
        assert_eq!(
            super::tests::cache(dir.path(), 2).read(&a, Entry::Original),
            None
        );
    }

    #[test]
    fn trimming_drops_the_originals_used_longest_ago_and_keeps_what_shows() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache(dir.path(), 1);
        let ids: Vec<AttachmentId> = (0..4).map(|n| id(&format!("3/{n}"))).collect();
        let long_ago = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        for (n, at) in ids.iter().enumerate() {
            cache.write(at, Entry::Original, &[0; 1000]).unwrap();
            cache.write(at, Entry::Shown, b"shown").unwrap();
            let file = fs::File::options()
                .append(true)
                .open(dir.path().join(cache.name(at, Entry::Original)))
                .unwrap();
            file.set_modified(long_ago + std::time::Duration::from_secs(n as u64))
                .unwrap();
        }
        // The first, read now, is the most recently used.
        assert!(cache.read(&ids[0], Entry::Original).is_some());
        let size = fs::metadata(dir.path().join(cache.name(&ids[0], Entry::Original)))
            .unwrap()
            .len();
        cache.trim(2 * size);
        let kept: Vec<bool> = ids
            .iter()
            .map(|at| cache.read(at, Entry::Original).is_some())
            .collect();
        assert_eq!(kept, [true, false, false, true]);
        assert!(ids.iter().all(|at| cache.read(at, Entry::Shown).is_some()));
    }

    #[test]
    fn forgetting_and_clearing_remove_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path(), &Zeroizing::new([1; 32])).unwrap();
        let at = id("3/a");
        cache.write(&at, Entry::Original, b"x").unwrap();
        cache.write(&at, Entry::Shown, b"y").unwrap();
        cache.forget(&at);
        assert_eq!(cache.read(&at, Entry::Original), None);
        assert_eq!(cache.read(&at, Entry::Shown), None);
        cache.write(&at, Entry::Original, b"x").unwrap();
        empty(dir.path()).unwrap();
        assert_eq!(cache.read(&at, Entry::Original), None);
        cache.write(&at, Entry::Original, b"x").unwrap();
        clear(dir.path()).unwrap();
        assert!(!dir.path().join(DIR).exists());
        clear(dir.path()).unwrap();
    }
}
