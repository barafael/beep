//! The local database's encryption key.
//!
//! The preferred source is the OS credential store (Secret Service on Linux, so GNOME Keyring
//! or KWallet; Credential Manager on Windows; Keychain on macOS), holding a random key. When
//! none is reachable, the key is derived from a user password with Argon2id. Either way the
//! result is a raw 256-bit SQLCipher key, so opening connections skips SQLCipher's own KDF.
//!
//! Which source a database uses is recorded next to it (with the salt, for passwords), so a
//! keyring that later goes missing is reported as such instead of silently making a new key.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use keyring_core::{CredentialStore, Entry, Error as KeyringError};
use zeroize::Zeroizing;

const SERVICE: &str = "langis";
/// Keyring entry of the default data directory.
pub const DEFAULT_ACCOUNT: &str = "database-key";

pub type Key = Zeroizing<[u8; 32]>;

#[derive(Debug, PartialEq, Eq)]
pub enum KeySource {
    Keyring,
    Password { salt: [u8; 16] },
}

impl KeySource {
    /// `None` when no source has been recorded yet.
    pub fn read(path: &Path) -> io::Result<Option<Self>> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let invalid = || io::Error::new(io::ErrorKind::InvalidData, format!("unrecognized {}", path.display()));
        match text.split_whitespace().collect::<Vec<_>>()[..] {
            ["keyring"] => Ok(Some(Self::Keyring)),
            ["password", salt] => {
                let mut bytes = [0; 16];
                hex::decode_to_slice(salt, &mut bytes).map_err(|_| invalid())?;
                Ok(Some(Self::Password { salt: bytes }))
            }
            _ => Err(invalid()),
        }
    }

    /// Writes the source atomically; see [`write_private`] for why that matters here.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let text = match self {
            Self::Keyring => "keyring\n".to_string(),
            Self::Password { salt } => format!("password {}\n", hex::encode(salt)),
        };
        write_private(path, text.as_bytes())
    }
}

/// Writes `bytes` to `path` so that no other user can read them, and never leaves a partially
/// written file behind. This file records how to derive the database key, so a truncated one
/// would lock the user out of their own message history instead of merely failing to open.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write as _;

    let dir = parent_of(path);
    let mut temp = tempfile::Builder::new().prefix(".langis-").tempfile_in(dir)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    // `tempfile` creates files as 0600, so the contents are never briefly readable by another
    // user, and `persist` renames over `path` in one step, which a reader never sees half-written.
    // Dropping without it deletes the temporary and leaves any previous file untouched.
    let _ = temp.persist(path).map_err(|error| error.error)?;
    Ok(())
}

/// Creates `dir` if it is missing and restricts it to its owner, so that on a shared machine
/// no other user can read the database kept inside it.
pub fn restrict_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    // `DirBuilder::mode` would only apply to a directory it creates, and tightening an existing
    // one is the point: a directory left world-readable by an older version is what this fixes.
    restrict(dir, 0o700)
}

/// Restricts `path` to its owner. Best effort: Windows has no equivalent of unix modes and the
/// user's profile directory already limits access, so there is nothing to do there.
pub fn restrict_file(path: &Path) -> io::Result<()> {
    restrict(path, 0o600)
}

#[cfg(unix)]
fn restrict(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let permissions = fs::Permissions::from_mode(mode);
    // Setting the mode is a second write; skip it when it is already right.
    if fs::metadata(path)?.permissions() != permissions {
        fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict(path: &Path, _mode: u32) -> io::Result<()> {
    let _ = path;
    Ok(())
}

/// Creates an empty file that only its owner can read, replacing whatever was at `path`.
///
/// For contents that arrive in pieces: [`write_private`] covers writing a buffer in one go. The
/// file is built under a temporary name and moved into place by [`PendingFile::persist`], so a
/// reader never sees a half-written file and the contents are never briefly readable by another
/// user. Dropping it instead leaves any previous file untouched.
pub fn write_private_file(path: &Path) -> io::Result<PendingFile> {
    let temp = tempfile::Builder::new().prefix(".langis-").tempfile_in(parent_of(path))?;
    Ok(PendingFile { temp, path: path.to_path_buf() })
}

/// The directory a file lives in, for writing a temporary beside it.
fn parent_of(path: &Path) -> &Path {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    }
}

/// A file that replaces its destination when it is closed. Dropping it discards the contents
/// instead, so a failed write leaves the previous file untouched.
pub struct PendingFile {
    temp: tempfile::NamedTempFile,
    path: PathBuf,
}

impl PendingFile {
    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        std::io::Write::write_all(&mut self.temp, bytes)
    }

    pub fn sync_all(&mut self) -> io::Result<()> {
        self.temp.as_file().sync_all()
    }

    /// Renames the file into place. Dropping without calling this deletes the temporary, leaving
    /// any previous file untouched.
    pub fn persist(self) -> io::Result<()> {
        let _ = self.temp.persist(&self.path).map_err(|error| error.error)?;
        Ok(())
    }
}

/// The key stored in the OS credential store, if there is a usable store.
pub enum KeyringKey {
    Found(Key),
    Missing,
    Unavailable(String),
}

/// `account` names the keyring entry; each data directory needs its own.
pub fn keyring_key(account: &str) -> KeyringKey {
    match entry(account).and_then(|entry| entry.get_secret()) {
        Ok(secret) => match <[u8; 32]>::try_from(&secret[..]) {
            Ok(key) => KeyringKey::Found(Zeroizing::new(key)),
            Err(_) => KeyringKey::Unavailable("the stored key has the wrong length".into()),
        },
        Err(KeyringError::NoEntry) => KeyringKey::Missing,
        Err(error) => KeyringKey::Unavailable(error.to_string()),
    }
}

/// Generates a key and stores it in the OS credential store; `Err` if there is no usable store.
pub fn create_keyring_key(account: &str) -> Result<Key, String> {
    let key = random_key();
    let entry = entry(account).map_err(|e| e.to_string())?;
    entry.set_secret(&key[..]).map_err(|e| e.to_string())?;
    // Read it back: a store that accepted the write but can't return it is no store at all.
    match entry.get_secret() {
        Ok(stored) if stored[..] == key[..] => Ok(key),
        Ok(_) => Err("the keyring returned a different key".into()),
        Err(error) => Err(error.to_string()),
    }
}

/// Argon2id cost. The key is handed to SQLCipher as a raw key, so no second KDF stands between
/// the password and the database: this is the only thing making a guessed password expensive.
/// `argon2::Params::DEFAULT` is 19 MiB and 2 passes, which OWASP calls the bare minimum; 64 MiB
/// and 3 passes costs about 60 ms on a current laptop, which is a large gain for an offline
/// attacker copying the database while the per-attempt cost stays unnoticeable here.
fn password_params() -> argon2::Params {
    const MEMORY_KIB: u32 = 64 * 1024;
    const PASSES: u32 = 3;
    const PARALLELISM: u32 = 1;
    argon2::Params::new(MEMORY_KIB, PASSES, PARALLELISM, None).expect("valid Argon2 parameters")
}

/// Derives the key for a password-protected database.
pub fn password_key(password: &str, salt: &[u8; 16]) -> Key {
    let mut key = Zeroizing::new([0; 32]);
    argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, password_params())
        .hash_password_into(password.as_bytes(), salt, &mut key[..])
        .expect("argon2 accepts a 16-byte salt and 32-byte output");
    key
}

pub fn random_salt() -> [u8; 16] {
    let mut salt = [0; 16];
    getrandom::fill(&mut salt).expect("no OS random number generator");
    salt
}

fn random_key() -> Key {
    let mut key = Zeroizing::new([0; 32]);
    getrandom::fill(&mut key[..]).expect("no OS random number generator");
    key
}

/// Value for SQLCipher's `PRAGMA key` that uses `key` as the raw encryption key.
pub fn sqlcipher_pragma(key: &Key) -> Zeroizing<String> {
    Zeroizing::new(format!("\"x'{}'\"", hex::encode_upper(&key[..])))
}

fn entry(account: &str) -> keyring_core::Result<Entry> {
    platform_store()?.build(SERVICE, account, None)
}

fn platform_store() -> keyring_core::Result<Arc<CredentialStore>> {
    #[cfg(target_os = "macos")]
    let store: Arc<CredentialStore> = apple_native_keyring_store::keychain::Store::new()?;
    #[cfg(windows)]
    let store: Arc<CredentialStore> = windows_native_keyring_store::Store::new()?;
    #[cfg(all(unix, not(target_os = "macos")))]
    let store: Arc<CredentialStore> = zbus_secret_service_keyring_store::Store::new()?;
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_source_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        assert_eq!(KeySource::read(&path).unwrap(), None);
        for source in [KeySource::Keyring, KeySource::Password { salt: [7; 16] }] {
            source.write(&path).unwrap();
            assert_eq!(KeySource::read(&path).unwrap(), Some(source));
        }
        fs::write(&path, "garbage").unwrap();
        assert!(KeySource::read(&path).is_err());
    }

    /// The streaming writer replaces the destination only when the caller persists. Giving up
    /// part-way must leave the previous file alone rather than a half-written one.
    #[test]
    fn a_pending_file_is_written_only_when_persisted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("archive");
        fs::write(&path, "old").unwrap();

        let mut file = write_private_file(&path).unwrap();
        file.write_all(b"new").unwrap();
        drop(file);
        assert_eq!(fs::read_to_string(&path).unwrap(), "old", "wrote a partial file");

        let mut file = write_private_file(&path).unwrap();
        file.write_all(b"new").unwrap();
        file.persist().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn password_key_depends_on_password_and_salt() {
        let key = password_key("hunter22", &[1; 16]);
        assert_eq!(*key, *password_key("hunter22", &[1; 16]));
        assert_ne!(*key, *password_key("hunter23", &[1; 16]));
        assert_ne!(*key, *password_key("hunter22", &[2; 16]));
    }

    /// Talks to the real OS credential store, read-only: `cargo test -- --ignored keyring`.
    #[test]
    #[ignore]
    fn keyring_is_reachable() {
        match keyring_key(DEFAULT_ACCOUNT) {
            KeyringKey::Found(_) | KeyringKey::Missing => {}
            KeyringKey::Unavailable(reason) => panic!("keyring unavailable: {reason}"),
        }
    }

    #[test]
    fn pragma_is_a_raw_key_literal() {
        let pragma = sqlcipher_pragma(&Zeroizing::new([0xab; 32]));
        assert_eq!(pragma.as_str(), format!("\"x'{}'\"", "AB".repeat(32)));
    }
}
