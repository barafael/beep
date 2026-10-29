//! The keys volume, which `provision` writes once and every other container reads: where it is,
//! the ports its URLs name, and what more than one tool reads from it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const DIR: &str = "/keys";

/// A file in the keys volume.
#[derive(Clone, Copy, strum::IntoStaticStr, strum::Display)]
pub enum KeyFile {
    /// The keys' CA, in PEM: the clients trust it (testbox/android/Containerfile builds Android's
    /// trust store from it too).
    #[strum(serialize = "tls-ca.pem")]
    TlsCa,
    /// The CA again, alone in a directory: what b33p's link previews trust beside the system's
    /// (`SSL_CERT_DIR`, testbox/b33p/pod.yaml), which reads every file in it.
    #[strum(serialize = "trusted/tls-ca.pem")]
    TrustedCa,
    /// The keys' certificate for `localhost`, and for [`PREVIEW_HOST`], in PEM.
    #[strum(serialize = "tls.crt")]
    TlsCertificate,
    #[strum(serialize = "tls.key")]
    TlsKey,
    /// The certificate as Jetty reads it, in Signal-Server and storage-service.
    #[strum(serialize = "tls.p12")]
    Keystore,
    /// Signal-Desktop's SIGNAL_CI_CONFIG, the server half: [`DesktopConfig`].
    #[strum(serialize = "desktop-config.json")]
    Desktop,
    /// The servers for libsignal-service (b33p, presage-cli): `SignalServers::Custom`.
    #[strum(serialize = "servers.toml")]
    Servers,
    /// CDN0's key for the `cdn0` tool: [`Cdn0Credentials`].
    #[strum(serialize = "cdn0.toml")]
    Cdn0,
    /// storage-service's configuration, which `provision` writes and the `storage` tool runs it with.
    #[strum(serialize = "storage-service.yml")]
    StorageService,
    /// Signal-Server's secrets bundle.
    #[strum(serialize = "secrets.yml")]
    Secrets,
    /// tus-server's environment: CDN3's shared secret.
    #[strum(serialize = "tus.env")]
    Tus,
    #[strum(serialize = "unidentified-delivery.certificate")]
    UnidentifiedDeliveryCertificate,
    /// Written last: the files before it are whole.
    #[strum(serialize = "complete")]
    Complete,
}

impl KeyFile {
    pub fn path(self) -> PathBuf {
        PathBuf::from(DIR).join(<&'static str>::from(self))
    }
}

/// Jetty's HTTP/2 port, for libsignal-net (Desktop, Android's REST).
pub const LIBSIGNAL_NET_PORT: u16 = 8443;
/// Jetty's HTTP/1.1 port, for libsignal-service (b33p, presage-cli).
pub const LIBSIGNAL_SERVICE_PORT: u16 = 8444;
/// Signal-Server's omnibus port: websocket and gRPC (Android's chat).
pub const OMNIBUS_PORT: u16 = 8445;
/// tus-server under `wrangler dev` (testbox/tus/Containerfile).
pub const CDN3_PORT: u16 = 8446;
/// Garage behind the `cdn0` tool.
pub const CDN0_PORT: u16 = 8447;

/// The page link previews are made of (testbox/tools `preview`): a name of the internet's at an
/// address of the internet's, as b33p fetches previews from nothing else (b33p-core preview.rs
/// `PublicOnly`). testbox/pod.yaml puts the address on the pod's loopback and the name on it;
/// 192.88.99.0/24 was the 6to4 relays' anycast block, given up (RFC 7526) and routed nowhere.
pub const PREVIEW_HOST: &str = "preview.b33p-tour.org";
pub const PREVIEW_ADDRESS: [u8; 4] = [192, 88, 99, 1];
pub const PREVIEW_PORT: u16 = 8450;

/// A page of the preview server, at `path` ("/page/3").
pub fn preview_url(path: &str) -> String {
    format!("https://{PREVIEW_HOST}:{PREVIEW_PORT}{path}")
}

/// storage-service (testbox/storage/Containerfile), for the storage service and groups.
pub const STORAGE_PORT: u16 = 8448;
/// storage-service's admin port: Dropwizard's default, 8081, is Signal-Server's in the pod.
pub const STORAGE_ADMIN_PORT: u16 = 8449;

/// The ports the clients reach the servers on over TLS, which `--capture` relays through the `tap`
/// tool to see what they say: Signal-Server's three, the CDNs' and the storage service's.
pub const TAPPED_PORTS: [u16; 6] = [
    LIBSIGNAL_NET_PORT,
    LIBSIGNAL_SERVICE_PORT,
    OMNIBUS_PORT,
    CDN3_PORT,
    CDN0_PORT,
    STORAGE_PORT,
];

/// Where `tap` listens for what was bound for `port`, one of [`TAPPED_PORTS`].
pub const fn tap_port(port: u16) -> u16 {
    port + 1000
}

/// The address `tap` reaches the servers from, on the pod's loopback: the redirect to it passes
/// what comes from here by.
pub const TAP_SOURCE: [u8; 4] = [127, 0, 0, 2];

/// Where `tap` writes what it relays (testbox/tap/pod.yaml).
pub const TAP_DIR: &str = "/tap";

/// An S3 bucket, by its name and region.
#[derive(Clone, Copy)]
pub struct Bucket {
    pub name: &'static str,
    pub region: &'static str,
}

/// CDN0's bucket on Garage, which Signal-Server and storage-service sign POST policies for, in
/// the region they name (test.yml's `cdn.region`).
pub const CDN0_BUCKET: Bucket = Bucket {
    name: "cdn",
    region: "us-west-2",
};

/// The Bigtable emulator storage-service keeps its data in.
pub struct Bigtable {
    /// On the clients' localhost, unencrypted gRPC as the emulator speaks it.
    pub address: &'static str,
    pub project: &'static str,
    pub instance: &'static str,
    /// storage-service's tables, each with the one column family its code writes
    /// (StorageItemsTable, StorageManifestsTable, GroupsTable, GroupLogTable; its tests create
    /// them the same way, StorageManagerTest, GroupsManagerTest).
    pub contacts: BigtableTable,
    pub manifests: BigtableTable,
    pub groups: BigtableTable,
    pub group_logs: BigtableTable,
}

impl Bigtable {
    pub fn tables(&self) -> [BigtableTable; 4] {
        [self.contacts, self.manifests, self.groups, self.group_logs]
    }
}

pub const BIGTABLE: Bigtable = Bigtable {
    address: "127.0.0.1:8086",
    project: "testbox",
    instance: "testbox",
    contacts: BigtableTable {
        name: "contacts",
        family: "c",
    },
    manifests: BigtableTable {
        name: "contact-manifests",
        family: "m",
    },
    groups: BigtableTable {
        name: "groups",
        family: "g",
    },
    group_logs: BigtableTable {
        name: "group-logs",
        family: "l",
    },
};

#[derive(Clone, Copy)]
pub struct BigtableTable {
    pub name: &'static str,
    pub family: &'static str,
}

/// A server on the clients' localhost, which the keys' certificate names.
pub fn localhost(port: u16) -> String {
    format!("https://localhost:{port}")
}

/// Where Desktop looks people up: contact discovery, which the box doesn't run. libsignal's local
/// environment names a mock enclave (rust/bridge/shared/testing/src/net_env.rs:61), for which
/// neither libsignal nor Desktop's packages/mock-server has a server.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DirectoryType {
    Cdsi,
}

/// What the `cdn0` tool imports into Garage: `cdn0.toml`, the same pair as the secrets bundle's
/// `cdn.accessKey` and `cdn.accessSecret`.
#[derive(Serialize, Deserialize)]
pub struct Cdn0Credentials {
    pub access_key: String,
    pub secret_key: String,
}

/// The base64 body of `pem`'s block labelled `label`.
pub fn pem_body(pem: &str, label: &str) -> Option<String> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let mut lines = pem.lines().skip_while(|line| *line != begin);
    lines.next()?;
    let mut body = String::new();
    for line in lines {
        if line == end {
            return Some(body);
        }
        body.push_str(line);
    }
    None
}

/// The CDNs by the numbers Signal's clients know them by; on the wire a map keyed by them.
#[derive(Serialize, Deserialize)]
pub struct CdnUrls {
    #[serde(rename = "0")]
    pub cdn0: String,
    #[serde(rename = "2")]
    pub cdn2: String,
    #[serde(rename = "3")]
    pub cdn3: String,
}

/// Signal-Desktop's SIGNAL_CI_CONFIG, the server half (the rest is the run's:
/// testbox/desktop/driver.ts), with the fields its mock-server tests set
/// (ts/test-mock/bootstrap.node.ts `#generateConfig`). `/keys/desktop-config.json`; the Android
/// driver takes its values from it too.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopConfig {
    pub server_url: String,
    pub storage_url: String,
    pub resources_url: String,
    pub sfu_url: String,
    pub cdn: CdnUrls,
    pub directory_type: DirectoryType,
    #[serde(rename = "directoryCDSIUrl")]
    pub directory_cdsi_url: String,
    pub certificate_authority: String,
    pub server_public_params: String,
    pub server_trust_roots: Vec<String>,
    pub generic_server_public_params: String,
    pub backup_server_public_params: String,
    pub updates_enabled: bool,
}
