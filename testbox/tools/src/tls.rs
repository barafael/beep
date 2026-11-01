//! The TLS the box's own servers serve with (`cdn0`, `preview`, `front`): the keys' certificate,
//! and with `--capture` their sessions' secrets written to the file `SSLKEYLOGFILE` names, in
//! the NSS key log format (testbox/capture/pod.yaml). rustls reads the variable itself; unset,
//! the secrets go nowhere.

use std::sync::Arc;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{KeyLogFile, ServerConfig};

use crate::keys::KeyFile;
use crate::{Describe, Error, Result};

/// A server's configuration with the keys' certificate and the key log, offering `alpn`.
pub fn server_config(alpn: &[&str]) -> Result<ServerConfig> {
    let certificates = CertificateDer::pem_file_iter(KeyFile::TlsCertificate.path())
        .describe("the keys' certificate")?
        .collect::<std::result::Result<Vec<_>, _>>()
        .describe("the keys' certificate")?;
    let key = PrivateKeyDer::from_pem_file(KeyFile::TlsKey.path()).describe("the keys' key")?;
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .describe("the keys' certificate")?;
    config.alpn_protocols = alpn
        .iter()
        .map(|protocol| protocol.as_bytes().to_vec())
        .collect();
    config.key_log = Arc::new(KeyLogFile::new());
    Ok(config)
}

/// rustls' provider, once: ring, as the keys are made with it (testbox/tools Cargo.toml).
pub fn install_provider() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_installed| Error::Failed("a TLS provider is installed already".to_owned()))
}

/// Where this server's secrets go, said once at its start.
pub fn say_where_secrets_go(tool: &str) {
    match std::env::var_os("SSLKEYLOGFILE") {
        Some(path) => eprintln!("{tool}: the sessions' secrets go to {}", path.display()),
        None => eprintln!("{tool}: SSLKEYLOGFILE unset, the sessions' secrets go nowhere"),
    }
}
