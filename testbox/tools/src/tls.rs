//! The TLS the box's own servers serve with (`cdn0`, `preview`, `front`): the keys' certificate,
//! and with `--capture` their sessions' secrets written to the file `SSLKEYLOGFILE` names, in
//! the NSS key log format (testbox/capture/pod.yaml). rustls reads the variable itself; unset,
//! the secrets go nowhere.

use std::path::PathBuf;
use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{KeyLogFile, ServerConfig};

use crate::keys::KeyFile;

/// The keys' certificate and key, which a server's configuration couldn't take.
#[derive(Debug, thiserror::Error)]
pub enum TlsError {
    #[error("reading the keys' certificate {}", path.display())]
    Certificate {
        path: PathBuf,
        #[source]
        source: rustls::pki_types::pem::Error,
    },
    #[error("reading the keys' key {}", path.display())]
    Key {
        path: PathBuf,
        #[source]
        source: rustls::pki_types::pem::Error,
    },
    /// The certificate and key, read, that rustls refuses as a pair.
    #[error("serving with the keys' certificate")]
    Config(#[source] rustls::Error),
    /// Another provider than ring, installed before.
    #[error("a TLS provider is installed already")]
    ProviderInstalled,
}

/// A server's configuration with the keys' certificate and the key log, offering `alpn`.
pub fn server_config(alpn: &[&str]) -> Result<ServerConfig, TlsError> {
    let path = KeyFile::TlsCertificate.path();
    let certificate = |source| TlsError::Certificate {
        path: path.clone(),
        source,
    };
    let certificates = CertificateDer::pem_file_iter(&path)
        .map_err(certificate)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(certificate)?;
    let path = KeyFile::TlsKey.path();
    let key =
        PrivateKeyDer::from_pem_file(&path).map_err(|source| TlsError::Key { path, source })?;
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(TlsError::Config)?;
    config.alpn_protocols = alpn
        .iter()
        .map(|protocol| protocol.as_bytes().to_vec())
        .collect();
    config.key_log = Arc::new(KeyLogFile::new());
    Ok(config)
}

/// rustls' provider, once: ring, as the keys are made with it (testbox/tools Cargo.toml).
pub fn install_provider() -> Result<(), TlsError> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_installed| TlsError::ProviderInstalled)
}

/// Where this server's secrets go, said once at its start.
pub fn say_where_secrets_go(tool: &str) {
    match std::env::var_os("SSLKEYLOGFILE") {
        Some(path) => eprintln!("{tool}: the sessions' secrets go to {}", path.display()),
        None => eprintln!("{tool}: SSLKEYLOGFILE unset, the sessions' secrets go nowhere"),
    }
}
