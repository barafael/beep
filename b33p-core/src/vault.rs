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

const SERVICE: &str = "b33p";
/// Keyring entry of the default data directory.
pub const DEFAULT_ACCOUNT: &str = "database-key";

pub type Key = Zeroizing<[u8; 32]>;

#[derive(Debug, PartialEq, Eq)]
pub enum KeySource {
    Keyring,
    Password { salt: [u8; 16], kdf: Kdf },
}

/// Argon2id cost of a password-derived key. Recorded with the salt, so making it stronger for
/// new databases never changes the key of an existing one (which would lock its owner out).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kdf {
    /// The `argon2` crate's default, 19 MiB and 2 passes: databases made before `V2`.
    V1,
    /// 64 MiB and 3 passes, about 60 ms on a current laptop. The key goes to SQLCipher raw, so
    /// this cost is all that stands between a stolen database file and a password guess.
    V2,
}

impl Kdf {
    /// What new databases use.
    pub const CURRENT: Self = Self::V2;
}

impl KeySource {
    /// `None` when no source has been recorded yet.
    pub fn read(path: &Path) -> io::Result<Option<Self>> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let invalid = || {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unrecognized {}", path.display()),
            )
        };
        match text.split_whitespace().collect::<Vec<_>>()[..] {
            ["keyring"] => Ok(Some(Self::Keyring)),
            [tag @ ("password" | "password2"), salt] => {
                let mut bytes = [0; 16];
                hex::decode_to_slice(salt, &mut bytes).map_err(|_| invalid())?;
                let kdf = if tag == "password" { Kdf::V1 } else { Kdf::V2 };
                Ok(Some(Self::Password { salt: bytes, kdf }))
            }
            _ => Err(invalid()),
        }
    }

    /// Writes the source atomically and private to its owner; see [`write_private`].
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let text = match self {
            Self::Keyring => "keyring\n".to_string(),
            Self::Password { salt, kdf: Kdf::V1 } => format!("password {}\n", hex::encode(salt)),
            Self::Password { salt, kdf: Kdf::V2 } => format!("password2 {}\n", hex::encode(salt)),
        };
        write_private(path, text.as_bytes())
    }
}

/// Writes `bytes` to `path` readable by its owner only, never leaving a half-written file: the
/// key source records how to derive the database key, so a truncated one would lock the user
/// out of their history.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = write_private_file(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    file.persist()
}

/// Creates `dir` if missing and restricts it to its owner, so other users on the machine can't
/// copy the database inside and attack its password offline. Also tightens a directory an
/// earlier version left world-readable.
pub fn restrict_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    restrict(dir, 0o700)
}

/// Restricts `path` to its owner.
pub fn restrict_file(path: &Path) -> io::Result<()> {
    restrict(path, 0o600)
}

#[cfg(unix)]
fn restrict(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    // The mode read back carries the file type bits too; only the permission bits matter.
    if fs::metadata(path)?.permissions().mode() & 0o777 != mode {
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    Ok(())
}

/// Windows has no unix modes; the user's profile directory already keeps others out.
#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

/// A file built under a temporary name beside `path` (created 0600 by `tempfile`) and renamed
/// over it by [`PendingFile::persist`], for contents that arrive in pieces. Dropping it instead
/// deletes the temporary and leaves any previous file untouched.
pub fn write_private_file(path: &Path) -> io::Result<PendingFile> {
    let dir = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let temp = tempfile::Builder::new().prefix(".b33p-").tempfile_in(dir)?;
    Ok(PendingFile {
        temp,
        path: path.to_path_buf(),
    })
}

pub struct PendingFile {
    temp: tempfile::NamedTempFile,
    path: PathBuf,
}

impl PendingFile {
    pub fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        io::Write::write_all(&mut self.temp, bytes)
    }

    pub fn sync_all(&mut self) -> io::Result<()> {
        self.temp.as_file().sync_all()
    }

    pub fn persist(self) -> io::Result<()> {
        self.temp
            .persist(&self.path)
            .map(drop)
            .map_err(|error| error.error)?;
        sync_parent(&self.path)
    }
}

/// Makes a rename into `path`'s directory durable: without it, a power cut right after can
/// bring back the old file (or none).
#[cfg(unix)]
pub fn sync_parent(path: &Path) -> io::Result<()> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => fs::File::open(parent)?.sync_all(),
        _ => fs::File::open(".")?.sync_all(),
    }
}

/// Windows can't open a directory as a file; NTFS journals the rename itself.
#[cfg(not(unix))]
pub fn sync_parent(_path: &Path) -> io::Result<()> {
    Ok(())
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

/// Why [`create_keyring_key`] couldn't store a key.
#[derive(Debug)]
pub enum NoKeyring {
    /// There is a credential store, but it is locked or refused access (a dismissed prompt):
    /// it may well work in a moment.
    Locked(String),
    /// No usable credential store at all.
    Absent(String),
}

impl std::fmt::Display for NoKeyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Locked(reason) | Self::Absent(reason) => f.write_str(reason),
        }
    }
}

impl From<KeyringError> for NoKeyring {
    fn from(error: KeyringError) -> Self {
        match error {
            KeyringError::NoStorageAccess(_) => Self::Locked(error.to_string()),
            error => Self::Absent(error.to_string()),
        }
    }
}

/// Generates a key and stores it in the OS credential store; `Err` if there is no usable store.
pub fn create_keyring_key(account: &str) -> Result<Key, NoKeyring> {
    let key = random_key();
    let entry = entry(account)?;
    entry.set_secret(&key[..])?;
    // Read it back: a store that accepted the write but can't return it is no store at all.
    match entry.get_secret() {
        Ok(stored) if stored[..] == key[..] => Ok(key),
        Ok(_) => Err(NoKeyring::Absent(
            "the keyring returned a different key".into(),
        )),
        Err(error) => Err(error.into()),
    }
}

/// Removes the key from the OS credential store; done if there was none.
pub fn delete_keyring_key(account: &str) -> Result<(), String> {
    match entry(account).and_then(|entry| entry.delete_credential()) {
        Ok(()) | Err(KeyringError::NoEntry) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// Derives the key for a password-protected database.
pub fn password_key(password: &str, salt: &[u8; 16], kdf: Kdf) -> Key {
    let argon2 = match kdf {
        Kdf::V1 => argon2::Argon2::default(),
        Kdf::V2 => {
            let params =
                argon2::Params::new(64 * 1024, 3, 1, None).expect("valid Argon2 parameters");
            argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params)
        }
    };
    let mut key = Zeroizing::new([0; 32]);
    argon2
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
    fn key_source_round_trips_privately() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        assert_eq!(KeySource::read(&path).unwrap(), None);
        let sources = [
            KeySource::Keyring,
            KeySource::Password {
                salt: [7; 16],
                kdf: Kdf::V1,
            },
            KeySource::Password {
                salt: [7; 16],
                kdf: Kdf::V2,
            },
        ];
        for source in sources {
            source.write(&path).unwrap();
            assert_eq!(KeySource::read(&path).unwrap(), Some(source));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let leftovers = fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(leftovers, 1, "temporary files left behind");
        fs::write(&path, "garbage").unwrap();
        assert!(KeySource::read(&path).is_err());
    }

    /// Databases made before the stronger parameters must keep opening with their password.
    #[test]
    fn existing_password_databases_keep_their_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("key");
        fs::write(&path, format!("password {}\n", hex::encode([1; 16]))).unwrap();
        let Some(KeySource::Password { salt, kdf }) = KeySource::read(&path).unwrap() else {
            panic!()
        };
        assert_eq!(kdf, Kdf::V1);
        let mut before = [0; 32];
        argon2::Argon2::default()
            .hash_password_into(b"hunter22", &salt, &mut before)
            .unwrap();
        assert_eq!(*password_key("hunter22", &salt, kdf), before);
        assert_ne!(*password_key("hunter22", &salt, Kdf::V2), before);
    }

    /// The keys of existing databases, as `argon2` 0.5 derived them: a new version of the crate
    /// (or of its defaults) must not change them, or their owners are locked out.
    #[test]
    fn password_keys_stay_the_same_across_versions() {
        let salt = [3; 16];
        assert_eq!(
            hex::encode(*password_key("correct horse", &salt, Kdf::V1)),
            "2e298dc521fa674e081f66a90e3a218fe56cc8f4c400232e6abe6ddae79ffe28"
        );
        assert_eq!(
            hex::encode(*password_key("correct horse", &salt, Kdf::V2)),
            "c78a0653834a28970cef3be1c13c77279a7a9d8ef13d6a55ad7f42e14eb03ddb"
        );
    }

    #[test]
    fn password_key_depends_on_password_and_salt() {
        // V1: cheap in debug builds, and the parameters don't matter here.
        let key = password_key("hunter22", &[1; 16], Kdf::V1);
        assert_eq!(*key, *password_key("hunter22", &[1; 16], Kdf::V1));
        assert_ne!(*key, *password_key("hunter23", &[1; 16], Kdf::V1));
        assert_ne!(*key, *password_key("hunter22", &[2; 16], Kdf::V1));
    }

    #[cfg(unix)]
    #[test]
    fn data_directory_is_made_private_even_if_it_existed() {
        use std::os::unix::fs::PermissionsExt;
        let parent = tempfile::tempdir().unwrap();
        let dir = parent.path().join("b33p");
        fs::create_dir_all(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        restrict_dir(&dir).unwrap();
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
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
