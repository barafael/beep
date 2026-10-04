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
use std::path::Path;
use std::sync::Arc;

use keyring_core::{CredentialStore, Entry, Error as KeyringError};
use zeroize::Zeroizing;

const SERVICE: &str = "langis";
const ACCOUNT: &str = "database-key";

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

    pub fn write(&self, path: &Path) -> io::Result<()> {
        let text = match self {
            Self::Keyring => "keyring\n".to_string(),
            Self::Password { salt } => format!("password {}\n", hex::encode(salt)),
        };
        fs::write(path, text)
    }
}

/// The key stored in the OS credential store, if there is a usable store.
pub enum KeyringKey {
    Found(Key),
    Missing,
    Unavailable(String),
}

pub fn keyring_key() -> KeyringKey {
    match entry().and_then(|entry| entry.get_secret()) {
        Ok(secret) => match <[u8; 32]>::try_from(&secret[..]) {
            Ok(key) => KeyringKey::Found(Zeroizing::new(key)),
            Err(_) => KeyringKey::Unavailable("the stored key has the wrong length".into()),
        },
        Err(KeyringError::NoEntry) => KeyringKey::Missing,
        Err(error) => KeyringKey::Unavailable(error.to_string()),
    }
}

/// Generates a key and stores it in the OS credential store; `Err` if there is no usable store.
pub fn create_keyring_key() -> Result<Key, String> {
    let key = random_key();
    let entry = entry().map_err(|e| e.to_string())?;
    entry.set_secret(&key[..]).map_err(|e| e.to_string())?;
    // Read it back: a store that accepted the write but can't return it is no store at all.
    match entry.get_secret() {
        Ok(stored) if stored[..] == key[..] => Ok(key),
        Ok(_) => Err("the keyring returned a different key".into()),
        Err(error) => Err(error.to_string()),
    }
}

/// Derives the key for a password-protected database.
pub fn password_key(password: &str, salt: &[u8; 16]) -> Key {
    let mut key = Zeroizing::new([0; 32]);
    argon2::Argon2::default()
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

fn entry() -> keyring_core::Result<Entry> {
    platform_store()?.build(SERVICE, ACCOUNT, None)
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
        let dir = std::env::temp_dir().join(format!("langis-vault-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("key");
        assert_eq!(KeySource::read(&path).unwrap(), None);
        for source in [KeySource::Keyring, KeySource::Password { salt: [7; 16] }] {
            source.write(&path).unwrap();
            assert_eq!(KeySource::read(&path).unwrap(), Some(source));
        }
        fs::write(&path, "garbage").unwrap();
        assert!(KeySource::read(&path).is_err());
        fs::remove_dir_all(&dir).unwrap();
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
        match keyring_key() {
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
