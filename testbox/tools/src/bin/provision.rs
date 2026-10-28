//! The server container's entrypoint.
//!
//! Once per `keys` volume it writes what the server and the clients must agree on, the keystore
//! and storage-service's configuration among it. On every start it makes sure the pre-key bucket
//! exists, writes the server's config (it depends on DynamoDB's tables), then runs Signal-Server,
//! passing its log on, writes the remote config once the server has started, and says so
//! (`testbox_tools::SERVER_READY`). `provision keys` only
//! writes the keys (the pod's init container, testbox/pod.yaml).
//!
//! The stores' addresses are their names in the pod, which resolve to its localhost
//! (testbox/pod.yaml `hostAliases`), with the Redis cluster's nodes on ports of their own
//! (`REDIS_CLUSTER_URI`). Nothing waits for the stores here, as kube play has no readiness or
//! startup probes: the pod restarts a server that found one missing (`restartPolicy: Always`).
//! Not the server: one that finds the Redis cluster still forming starts all the same, and its
//! cluster client stays disconnected for a while after it formed, its requests failing ("Currently
//! not connected"). So the cluster is checked here first, as the pre-key bucket and DynamoDB are.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair as TlsKeyPair, KeyUsagePurpose, SanType,
};
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};
use strum::VariantArray;
use testbox_tools::keys::{self, Cdn0Credentials, DesktopConfig, KeyFile};
use testbox_tools::process::{spawn_logged, supervise, wait_for};
use testbox_tools::{Context, Describe, Error, Result, WithContext, read, write};
use tokio::process::Command;

const SERVER_CONFIG: &str = "/tmp/server.yml";
/// The table LocalDynamoDbFactory creates for remote config (DynamoDbExtensionSchema.java
/// `REMOTE_CONFIGS`); test.yml names `remote_config_test`, which nothing creates.
const REMOTE_CONFIG_TABLE: &str = "remote_configs_test";
/// A request of Desktop's libsignal-net, as its `grpc.` remote config key names it
/// (ts/RemoteConfig.dom.ts).
#[derive(Clone, Copy, strum::IntoStaticStr, strum::VariantArray)]
enum DesktopGrpcRequest {
    #[strum(serialize = "AccountsAnonymousCheckAccountExistence.2")]
    AccountsAnonymousCheckAccountExistence,
    AccountsAnonymousLookupUsernameHash,
    #[strum(serialize = "AccountsAnonymousLookupUsernameLink.2")]
    AccountsAnonymousLookupUsernameLink,
    AttachmentsGetUploadForm,
    BackupsAnonymousGetUploadForm,
    KeyTransparencyQueryServiceSearchV2,
    #[strum(serialize = "MessagesAnonymousSendMultiRecipientMessage.2")]
    MessagesAnonymousSendMultiRecipientMessage,
    MessagesAnonymousSendSingleRecipientMessage,
    MessagesSendMessage,
}

impl DesktopGrpcRequest {
    /// Its key in the server's remote config table.
    fn remote_config_key(self) -> String {
        let name: &'static str = self.into();
        format!("desktop.libsignalNet.grpc.{name}")
    }
}
const TEST_CONFIG: &str = "/opt/signal/test-classes/config";
const SERVER_MAIN: &str = "org.whispersystems.textsecuregcm.WhisperServerService";
/// The paged one-time KEM pre-keys' bucket, on `localstack` (testbox/pod.yaml): the SDK addresses
/// the bucket by host name, the alias prekeys.s3.box.
struct PreKeyBucket {
    endpoint: &'static str,
    name: &'static str,
}

const PREKEY_BUCKET: PreKeyBucket = PreKeyBucket {
    endpoint: "http://s3.box:4566",
    name: "prekeys",
};
const REDIS_URI: &str = "redis://redis:6379";
/// The cluster's first node, in the pod's one port space (testbox/pod.yaml `redis-0`).
const REDIS_CLUSTER_URI: &str = "redis://redis-0:7000";
/// The same node, for [`redis_cluster_formed`].
const REDIS_CLUSTER_NODE: &str = "redis-0:7000";
/// How long a check of the cluster may take.
const REDIS_PATIENCE: Duration = Duration::from_secs(5);
const DYNAMODB_ENDPOINT: &str = "http://dynamodb:8000";
/// Longer than any start of the server's here (about a minute), so that a log without Jetty's
/// start line fails the container instead of leaving Desktop without its remote config.
const SERVER_PATIENCE: Duration = Duration::from_secs(600);
/// Ten years: the box's keys outlive any run.
const VALIDITY_DAYS: i64 = 3650;

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let outcome = match testbox_tools::argument("provision [keys]") {
        Ok(None) => run(false).await,
        Ok(Some(argument)) if argument == "keys" => run(true).await,
        Ok(Some(other)) => Err(Error::Failed(format!(
            "usage: provision [keys], not {other}"
        ))),
        Err(error) => Err(error),
    };
    testbox_tools::exit("provision", outcome)
}

async fn run(keys_only: bool) -> Result<()> {
    if !KeyFile::Complete.path().exists() {
        if Path::new(BUILT_KEYS).exists() {
            copy_built_keys()?;
        } else {
            write_keys()?;
        }
    }
    if keys_only {
        return Ok(());
    }
    let secrets_path = KeyFile::Secrets.path();
    let secrets: Secrets =
        serde_yaml::from_str(&read(&secrets_path)?).describe(secrets_path.display().to_string())?;
    let password = keystore_password(&secrets)?;
    let http = reqwest::Client::new();
    redis_cluster_formed()?;
    let ((), has_tables) =
        tokio::try_join!(ensure_prekey_bucket(&http), dynamodb_has_tables(&http))?;
    write_server_config(password, !has_tables)?;
    run_server(&http).await
}

/// The secrets bundle: every value a string (test-secrets-bundle.yml).
type Secrets = BTreeMap<String, String>;

fn secret<'a>(secrets: &'a Secrets, name: &str) -> Result<&'a str> {
    secrets
        .get(name)
        .map(String::as_str)
        .ok_or_else(|| Error::Failed(format!("no {name} in the secrets bundle")))
}

/// What the keys need of test.yml.
#[derive(Deserialize)]
struct TestConfig {
    #[serde(rename = "groupsZkConfig")]
    groups_zk_config: GroupsZkConfig,
}

#[derive(Deserialize)]
struct GroupsZkConfig {
    #[serde(rename = "serverPublic")]
    server_public: String,
}

/// libsignal-service's `ServiceConfiguration`, serialized (libsignal-service-rs
/// `configuration.rs`): what presage-cli's `--servers-file` and b33p's `B33P_SIGNAL_SERVERS`
/// read. The table last, as TOML has it.
#[derive(Serialize)]
struct ServersFile {
    service_url: String,
    storage_url: String,
    certificate_authority: String,
    unidentified_sender_trust_roots: Vec<String>,
    zkgroup_server_public_params: String,
    cdn_urls: keys::CdnUrls,
}

/// The keys built into the image (testbox/server/Containerfile, its `keys` stage): every image
/// built from it agrees with them, the Android image's APK among them. Every box from one image
/// has the same keys, which only box-configured clients trust: public only if an image is pushed.
const BUILT_KEYS: &str = "/opt/testbox/keys";

/// The image's keys into the volume, `complete` last: a copy cut short is written again.
fn copy_built_keys() -> Result<()> {
    copy_dir(Path::new(BUILT_KEYS), Path::new(keys::DIR))?;
    write(&KeyFile::Complete.path(), "")
}

/// The files of `from` into `to`, and its directories with theirs (`trusted/`), but `complete`.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    for entry in std::fs::read_dir(from).context(from.display().to_string())? {
        let from = entry.context(from.display().to_string())?.path();
        let name = from
            .file_name()
            .ok_or_else(|| Error::Failed(format!("{} has no name", from.display())))?;
        if name == <&str>::from(KeyFile::Complete) {
            continue;
        }
        let to = to.join(name);
        if from.is_dir() {
            std::fs::create_dir_all(&to).context(to.display().to_string())?;
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)
                .with_context(|| format!("{} to {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn write_keys() -> Result<()> {
    let ca_pem = write_tls()?;

    // Sealed sender: the test bundle's key (test-secrets-bundle.yml
    // `unidentifiedDelivery.privateKey`) has no published trust root, so a trust root of our own
    // signs a certificate of our own, as Signal-Server's `certificate` command makes them
    // (CertificateCommand.java): the keys and the certificate in base64.
    let mut rng = rand::rng();
    let trust_root = libsignal_protocol::KeyPair::generate(&mut rng);
    let server_key = libsignal_protocol::KeyPair::generate(&mut rng);
    let certificate = libsignal_protocol::ServerCertificate::new(
        1,
        server_key.public_key,
        &trust_root.private_key,
        &mut rng,
    )
    .describe("the sealed-sender certificate")?;
    let trust_root_public = BASE64.encode(trust_root.public_key.serialize());

    let bundle_path = Path::new(TEST_CONFIG).join("test-secrets-bundle.yml");
    let mut secrets: Secrets =
        serde_yaml::from_str(&read(&bundle_path)?).describe(bundle_path.display().to_string())?;
    secrets.insert(
        "unidentifiedDelivery.privateKey".to_owned(),
        BASE64.encode(server_key.private_key.serialize()),
    );
    // CDN0's access key, which Garage takes only in its own shape (testbox/tools `cdn0` imports
    // it): `GK` and 12 bytes in hex, and a 32-byte secret in hex.
    let cdn0 = Cdn0Credentials {
        access_key: format!("GK{}", hex::encode(rand::random::<[u8; 12]>())),
        secret_key: hex::encode(rand::random::<[u8; 32]>()),
    };
    secrets.insert("cdn.accessKey".to_owned(), cdn0.access_key.clone());
    secrets.insert("cdn.accessSecret".to_owned(), cdn0.secret_key.clone());
    write(
        &KeyFile::Secrets.path(),
        serde_yaml::to_string(&secrets).describe("the secrets")?,
    )?;
    write(
        &KeyFile::Cdn0.path(),
        toml::to_string(&cdn0).describe(KeyFile::Cdn0.to_string())?,
    )?;
    // CDN3's half of the shared secret, as tus-server's dev server reads it (`wrangler dev
    // --env-file`, testbox/tus/Containerfile): base64 on both sides (JwtGenerator's secret bytes;
    // tus-server src/index.ts `authenticateToken`).
    write(
        &KeyFile::Tus.path(),
        format!(
            "SHARED_AUTH_SECRET=\"{}\"\n",
            secret(&secrets, "tus.userAuthenticationTokenSharedSecret")?
        ),
    )?;
    write(
        &KeyFile::UnidentifiedDeliveryCertificate.path(),
        BASE64.encode(
            certificate
                .serialized()
                .describe("the sealed-sender certificate")?,
        ),
    )?;

    let test_config_path = Path::new(TEST_CONFIG).join("test.yml");
    let test_config: TestConfig = serde_yaml::from_str(&read(&test_config_path)?)
        .describe(test_config_path.display().to_string())?;
    let zkgroup_public = test_config.groups_zk_config.server_public;

    // The servers for libsignal-service (b33p, presage-cli): `SignalServers::Custom`. The zkgroup
    // parameters are the test configuration's own pair (test.yml `groupsZkConfig`).
    let libsignal_service_url = keys::localhost(keys::LIBSIGNAL_SERVICE_PORT);
    let servers = ServersFile {
        service_url: libsignal_service_url.clone(),
        storage_url: keys::localhost(keys::STORAGE_PORT),
        certificate_authority: ca_pem.clone(),
        unidentified_sender_trust_roots: vec![trust_root_public.clone()],
        zkgroup_server_public_params: zkgroup_public.clone(),
        cdn_urls: cdn_urls(&libsignal_service_url),
    };
    write(
        &KeyFile::Servers.path(),
        toml::to_string(&servers).describe(KeyFile::Servers.to_string())?,
    )?;

    // `localhost` with a CA makes Desktop use libsignal-net's local override
    // (ts/textsecure/preconnect.preload.ts `isMockServer`). The generic parameters are the public
    // halves of the secrets the server uses for call links (callingZkConfig, CallLinkController)
    // and backups (chatZkConfig, BackupAuthManager).
    let libsignal_net_url = keys::localhost(keys::LIBSIGNAL_NET_PORT);
    let desktop = DesktopConfig {
        server_url: libsignal_net_url.clone(),
        storage_url: keys::localhost(keys::STORAGE_PORT),
        resources_url: libsignal_net_url.clone(),
        sfu_url: libsignal_net_url.clone(),
        cdn: cdn_urls(&libsignal_net_url),
        directory_type: keys::DirectoryType::Cdsi,
        directory_cdsi_url: libsignal_net_url,
        certificate_authority: ca_pem,
        server_public_params: zkgroup_public,
        server_trust_roots: vec![trust_root_public],
        generic_server_public_params: generic_public_params(secret(
            &secrets,
            "callingZkConfigV101.serverSecret",
        )?)?,
        backup_server_public_params: generic_public_params(secret(
            &secrets,
            "chatZkConfig.serverSecret",
        )?)?,
        updates_enabled: false,
    };
    write(
        &KeyFile::Desktop.path(),
        serde_json::to_string_pretty(&desktop).describe(KeyFile::Desktop.to_string())?,
    )?;

    let password = keystore_password(&secrets)?;
    write_keystore(password)?;
    write_storage_service_config(&secrets, &cdn0, password)?;

    readable(Path::new(keys::DIR))?;
    write(&KeyFile::Complete.path(), "")
}

/// Every file in `directory` readable by every client container, whichever user it runs as, and
/// its directories open to them.
fn readable(directory: &Path) -> Result<()> {
    for entry in std::fs::read_dir(directory).context(directory.display().to_string())? {
        let path = entry.context(directory.display().to_string())?.path();
        let mode = if path.is_dir() { 0o755 } else { 0o644 };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
            .context(path.display().to_string())?;
        if path.is_dir() {
            readable(&path)?;
        }
    }
    Ok(())
}

/// CDN0: Garage behind testbox/tools `cdn0`. CDN2: the server's own. CDN3: tus-server under
/// `wrangler dev` (testbox/tus), uploads under /upload, downloads under /attachments.
fn cdn_urls(service_url: &str) -> keys::CdnUrls {
    keys::CdnUrls {
        cdn0: keys::localhost(keys::CDN0_PORT),
        cdn2: service_url.to_owned(),
        cdn3: keys::localhost(keys::CDN3_PORT),
    }
}

/// The public half of a base64 `GenericServerSecretParams`, in base64.
fn generic_public_params(secret: &str) -> Result<String> {
    let bytes = BASE64.decode(secret).describe("a zkgroup secret")?;
    let params: zkgroup::generic_server_params::GenericServerSecretParams =
        zkgroup::deserialize(&bytes).describe("a zkgroup secret")?;
    Ok(BASE64.encode(zkgroup::serialize(&params.get_public_params())))
}

/// A CA and a certificate for `localhost`: libsignal-service pins one CA (push_service/mod.rs
/// `tls_certs_only`); libsignal-net's local override connects to `localhost` only (net_env.rs
/// `hostname: "localhost"`). Returns the CA, in PEM.
fn write_tls() -> Result<String> {
    let now = time::OffsetDateTime::now_utc();
    let until = now + time::Duration::days(VALIDITY_DAYS);

    let ca_key =
        TlsKeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).describe("the CA's key")?;
    let mut ca_params = CertificateParams::default();
    ca_params.distinguished_name = name("b33p testbox CA");
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    ca_params.not_before = now;
    ca_params.not_after = until;
    let ca = ca_params.self_signed(&ca_key).describe("the CA")?;

    let server_key =
        TlsKeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).describe("the server's key")?;
    let mut server_params = CertificateParams::default();
    server_params.distinguished_name = name("localhost");
    server_params.subject_alt_names = vec![
        SanType::DnsName("localhost".try_into().describe("localhost")?),
        SanType::IpAddress(std::net::IpAddr::from([127, 0, 0, 1])),
        SanType::DnsName(keys::PREVIEW_HOST.try_into().describe(keys::PREVIEW_HOST)?),
    ];
    server_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    server_params.not_before = now;
    server_params.not_after = until;
    let issuer = Issuer::from_params(&ca_params, &ca_key);
    let server = server_params
        .signed_by(&server_key, &issuer)
        .describe("the server's certificate")?;

    let ca_pem = ca.pem();
    write(&KeyFile::TlsCa.path(), &ca_pem)?;
    let trusted = KeyFile::TrustedCa.path();
    if let Some(directory) = trusted.parent() {
        std::fs::create_dir_all(directory).context(directory.display().to_string())?;
    }
    write(&trusted, &ca_pem)?;
    write(&KeyFile::TlsCertificate.path(), server.pem())?;
    write(&KeyFile::TlsKey.path(), server_key.serialize_pem())?;
    Ok(ca_pem)
}

fn name(common_name: &str) -> DistinguishedName {
    let mut name = DistinguishedName::new();
    name.push(DnType::CommonName, common_name);
    name
}

/// The secrets bundle's `tlsKeyStore.password`: the server opens every TLS connector's keystore
/// with it for its certificate-expiry metrics (TlsCertificateExpirationUtil.configureMetrics),
/// whatever the connector's own `keyStorePassword` says.
fn keystore_password(secrets: &Secrets) -> Result<&str> {
    secret(secrets, "tlsKeyStore.password")
}

/// The keys' certificate and key as Jetty reads them, once: Signal-Server and storage-service
/// both serve with it.
fn write_keystore(password: &str) -> Result<()> {
    let certificate = pem_der(&read(&KeyFile::TlsCertificate.path())?, "CERTIFICATE")?;
    let key = pem_der(&read(&KeyFile::TlsKey.path())?, "PRIVATE KEY")?;
    let chain =
        p12_keystore::PrivateKeyChain::new(
            rand::random::<[u8; 20]>().as_slice(),
            p12_keystore::PrivateKey::from_der(&key).describe("the server's key")?,
            [p12_keystore::Certificate::from_der(&certificate)
                .describe("the server's certificate")?],
        );
    let mut store = p12_keystore::KeyStore::new();
    store.add_entry(
        "server",
        p12_keystore::KeyStoreEntry::PrivateKeyChain(chain),
    );
    let bytes = store.writer(password).write().describe("the keystore")?;
    write(&KeyFile::Keystore.path(), bytes)
}

/// storage-service's configuration (StorageServiceConfiguration.java), sharing Signal-Server's
/// secrets where the two must agree: the storage credentials Signal-Server hands out
/// (`storageService.userAuthenticationTokenSharedSecret`, which storage-service reads as hex,
/// AuthenticationConfiguration.java) and the zkgroup secret behind the group credentials
/// (`groupsZkConfig`, `zkConfig-libsignal-0.42.serverSecret`). Group avatars go to CDN0 with the
/// CDN's key. It serves TLS on the clients' localhost with the keys' certificate, HTTP/1.1, as
/// every client's storage requests are REST.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageServiceConfig {
    server: DropwizardServer,
    bigtable: BigtableConfig,
    authentication: Authentication,
    zk_config: ZkConfig,
    cdn: StorageCdn,
    group: GroupConfig,
    open_telemetry: OpenTelemetry,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DropwizardServer {
    application_connectors: [Connector; 1],
    admin_connectors: [AdminConnector; 1],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminConnector {
    #[serde(rename = "type")]
    kind: ConnectorType,
    bind_host: &'static str,
    port: u16,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BigtableConfig {
    project_id: &'static str,
    instance_id: &'static str,
    contacts_table_id: &'static str,
    contact_manifests_table_id: &'static str,
    groups_table_id: &'static str,
    group_logs_table_id: &'static str,
}

#[derive(Serialize)]
struct Authentication {
    /// Hex.
    key: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ZkConfig {
    /// Base64, as Jackson reads a `byte[]`.
    server_secret: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StorageCdn {
    access_key: String,
    access_secret: String,
    bucket: &'static str,
    region: &'static str,
}

/// Signal's limits (Signal-Android `RemoteConfig` `groupLimits`), and a secret of the box's own
/// for the credentials it hands to external services (call links).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GroupConfig {
    max_group_size: u32,
    max_group_title_length_bytes: u32,
    max_group_description_length_bytes: u32,
    /// Hex, 32 bytes.
    external_service_secret: String,
}

#[derive(Serialize)]
struct OpenTelemetry {
    enabled: bool,
}

fn write_storage_service_config(
    secrets: &Secrets,
    cdn0: &Cdn0Credentials,
    password: &str,
) -> Result<()> {
    let storage_secret = BASE64
        .decode(secret(
            secrets,
            "storageService.userAuthenticationTokenSharedSecret",
        )?)
        .describe("the storage service's shared secret")?;
    let config = StorageServiceConfig {
        server: DropwizardServer {
            application_connectors: [tls_connector(
                ConnectorType::Https,
                keys::STORAGE_PORT,
                password,
            )],
            admin_connectors: [AdminConnector {
                kind: ConnectorType::Http,
                bind_host: "127.0.0.1",
                port: keys::STORAGE_ADMIN_PORT,
            }],
        },
        bigtable: BigtableConfig {
            project_id: keys::BIGTABLE.project,
            instance_id: keys::BIGTABLE.instance,
            contacts_table_id: keys::BIGTABLE.contacts.name,
            contact_manifests_table_id: keys::BIGTABLE.manifests.name,
            groups_table_id: keys::BIGTABLE.groups.name,
            group_logs_table_id: keys::BIGTABLE.group_logs.name,
        },
        authentication: Authentication {
            key: hex::encode(&storage_secret),
        },
        zk_config: ZkConfig {
            server_secret: secret(secrets, "zkConfig-libsignal-0.42.serverSecret")?.to_owned(),
        },
        cdn: StorageCdn {
            access_key: cdn0.access_key.clone(),
            access_secret: cdn0.secret_key.clone(),
            bucket: keys::CDN0_BUCKET.name,
            region: keys::CDN0_BUCKET.region,
        },
        group: GroupConfig {
            max_group_size: 1001,
            max_group_title_length_bytes: 1024,
            max_group_description_length_bytes: 8192,
            external_service_secret: hex::encode(rand::random::<[u8; 32]>()),
        },
        open_telemetry: OpenTelemetry { enabled: false },
    };
    write(
        &KeyFile::StorageService.path(),
        serde_yaml::to_string(&config).describe("storage-service's configuration")?,
    )
}

/// The DER inside a PEM block labelled `label`.
fn pem_der(pem: &str, label: &str) -> Result<Vec<u8>> {
    let body =
        keys::pem_body(pem, label).ok_or_else(|| Error::Failed(format!("no PEM {label}")))?;
    BASE64.decode(body).describe(format!("a PEM {label}"))
}

/// Dropwizard's connector types the box uses (`server.applicationConnectors[].type`).
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
enum ConnectorType {
    Http,
    Https,
    /// HTTP/2 over TLS.
    H2,
}

/// A connector serving TLS with the keys' keystore.
fn tls_connector(kind: ConnectorType, port: u16, password: &str) -> Connector {
    Connector {
        kind,
        port,
        key_store_path: KeyFile::Keystore.path(),
        key_store_password: password.to_owned(),
        key_store_type: "PKCS12",
        validate_certs: false,
        supported_protocols: None,
        supported_cipher_suites: None,
    }
}

/// A TLS connector of Dropwizard's (`server.applicationConnectors`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Connector {
    #[serde(rename = "type")]
    kind: ConnectorType,
    port: u16,
    key_store_path: PathBuf,
    key_store_password: String,
    key_store_type: &'static str,
    validate_certs: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    supported_protocols: Option<&'static [&'static str]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    supported_cipher_suites: Option<&'static [&'static str]>,
}

/// Signal-Server's own test.yml, except where its `type: local` factories would start stores
/// through Testcontainers (redis/RedisServerExtension.java, redis/RedisClusterExtension.java,
/// storage/DynamoDbExtension.java), a container runtime the box doesn't give the server. Those
/// stores are the pod's containers, from the images Signal-Server's pom.xml pins for its tests.
///
/// test.yml is Signal-Server's, hundreds of keys we don't own: it stays a YAML document, and the
/// changes go in at their paths.
fn write_server_config(password: &str, init_tables: bool) -> Result<()> {
    let path = Path::new(TEST_CONFIG).join("test.yml");
    let mut config: Value =
        serde_yaml::from_str(&read(&path)?).describe(path.display().to_string())?;

    // Without `type`, the production factories (`defaultImpl` on FaultTolerantRedisClusterFactory
    // and FaultTolerantRedisClientFactory). One cluster for all four, as
    // LocalFaultTolerantRedisClusterFactory shares one RedisClusterExtension.
    for path in [
        &["cacheCluster"][..],
        &["pushSchedulerCluster"],
        &["rateLimitersCluster"],
        &["messageCache", "cluster"],
    ] {
        set(
            &mut config,
            path,
            mapping("configurationUri", REDIS_CLUSTER_URI),
        )?;
    }
    set(&mut config, &["pubsub"], mapping("uri", REDIS_URI))?;
    // `overrides.endpoint` skips LocalDynamoDbFactory's container (DynamoDbExtension.startServer).
    // DynamoDB Local keeps the tables across a restart of the server, which fails on creating
    // them again ("Cannot create preexisting table"): `initTables` only while there are none.
    set(
        &mut config,
        &["dynamoDbClient", "overrides"],
        mapping("endpoint", DYNAMODB_ENDPOINT),
    )?;
    set(
        &mut config,
        &["dynamoDbClient", "initTables"],
        Value::Bool(init_tables),
    )?;
    set(
        &mut config,
        &["dynamoDbTables", "remoteConfig"],
        mapping("tableName", REMOTE_CONFIG_TABLE),
    )?;
    // WhisperServerService builds the pre-key store's S3 client from `endpointOverride`;
    // LocalStack rejects the name `preKeyBucket`.
    set(
        &mut config,
        &["pagedSingleUseKEMPreKeyStore", "bucket"],
        PREKEY_BUCKET.name.into(),
    )?;
    set(
        &mut config,
        &["pagedSingleUseKEMPreKeyStore", "endpointOverride"],
        PREKEY_BUCKET.endpoint.into(),
    )?;
    set(
        &mut config,
        &["unidentifiedDelivery", "certificate"],
        read(&KeyFile::UnidentifiedDeliveryCertificate.path())?.into(),
    )?;

    // TLS in the server itself, on two ports. libsignal-net (Signal-Desktop, Android) opens its
    // websocket over HTTP/2 (RFC 8441 extended CONNECT), which a Caddy proxy in front didn't pass
    // ("ws failed: websocket error" in Desktop's log): LIBSIGNAL_NET_PORT offers h2.
    // libsignal-service's websocket can't upgrade over h2 once ALPN picks it ("Websocket error:
    // reqwest error"): LIBSIGNAL_SERVICE_PORT offers HTTP/1.1 only.
    let tls = |kind, port| tls_connector(kind, port, password);
    let connectors = [
        Connector {
            // Dropwizard's h2 default is TLS 1.2 with one RSA suite, which the keys' ECDSA
            // certificate can't serve ("handshake failure"). That suite must stay listed, as
            // Dropwizard insists ("HTTP/2 server configuration must include cipher:
            // TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256"; RFC 7540 §9.2.2).
            supported_protocols: Some(&["TLSv1.3", "TLSv1.2"]),
            supported_cipher_suites: Some(&[
                "TLS_AES_128_GCM_SHA256",
                "TLS_AES_256_GCM_SHA384",
                "TLS_CHACHA20_POLY1305_SHA256",
                "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256",
                "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384",
                "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256",
            ]),
            ..tls(ConnectorType::H2, keys::LIBSIGNAL_NET_PORT)
        },
        tls(ConnectorType::Https, keys::LIBSIGNAL_SERVICE_PORT),
    ];
    let Value::Sequence(existing) = at(&mut config, &["server", "applicationConnectors"])? else {
        return Err(Error::Failed(
            "test.yml's server.applicationConnectors isn't a list".to_owned(),
        ));
    };
    for connector in &connectors {
        existing.push(yaml(connector)?);
    }

    // libsignal-net sends a request as gRPC on its HTTP/2 chat connection unless remote config
    // names it "ws" (libsignal rust/bridge/shared/types/src/net.rs `chat_grpc_overrides`); Jetty
    // answers those 404 (Signal-Android's linked devices: "unexpected gRPC status"). Signal-Server
    // answers them on its omnibus HTTP/2 server, which passes the websocket paths on to
    // `websocketPort` (test.yml's h2c connector) and the rest to its gRPC services
    // (WhisperServerService.java `OmnibusRouter`). test.yml runs it as plaintext h2c; with TLS it
    // takes its certificates from `tlsKeyStore.path` (SniMapper.buildSniMapping).
    set(
        &mut config,
        &["tlsKeyStore", "path"],
        KeyFile::Keystore.path().display().to_string().into(),
    )?;
    set(
        &mut config,
        &["grpc", "port"],
        Value::from(keys::OMNIBUS_PORT),
    )?;
    set(&mut config, &["grpc", "h2c"], Value::Bool(false))?;
    // Attachments: every upload form names CDN3, which the box runs; the
    // default enrollment is 0 (DynamicExperimentEnrollmentConfiguration.java), and CDN2 is GCS.
    // test.yml caps uploads at 1024 bytes; 100 MiB is room for any test file.
    set(
        &mut config,
        &["tus", "uploadUri"],
        format!("{}/upload", keys::localhost(keys::CDN3_PORT)).into(),
    )?;
    set(
        &mut config,
        &["attachments", "maxAttachmentUploadSizeInBytes"],
        Value::from(100 * 1024 * 1024),
    )?;
    // The dynamic configuration is a YAML document in a string.
    let Value::String(dynamic) = at(&mut config, &["dynamicConfig", "object"])? else {
        return Err(Error::Failed(
            "test.yml's dynamicConfig.object isn't a string".to_owned(),
        ));
    };
    let mut dynamic_config: Value =
        serde_yaml::from_str(dynamic).describe("dynamicConfig.object")?;
    set(
        &mut dynamic_config,
        &["experiments"],
        mapping("cdn3", mapping("enrollmentPercentage", 100)),
    )?;
    let dynamic_text = serde_yaml::to_string(&dynamic_config).describe("dynamicConfig.object")?;
    set(
        &mut config,
        &["dynamicConfig", "object"],
        dynamic_text.into(),
    )?;

    write(
        Path::new(SERVER_CONFIG),
        serde_yaml::to_string(&config).describe("the server's configuration")?,
    )
}

fn yaml(value: &impl Serialize) -> Result<Value> {
    serde_yaml::to_value(value).describe("a configuration value")
}

/// `{key: value}`.
fn mapping(key: &str, value: impl Into<Value>) -> Value {
    Value::Mapping(Mapping::from_iter([(Value::from(key), value.into())]))
}

/// The value at `path`, which must exist.
fn at<'a>(root: &'a mut Value, path: &[&str]) -> Result<&'a mut Value> {
    path.iter().try_fold(root, |value, key| {
        value
            .get_mut(*key)
            .ok_or_else(|| Error::Failed(format!("test.yml has no {}", path.join("."))))
    })
}

/// Sets `path`'s last key in the mapping its other keys lead to, which must exist.
fn set(root: &mut Value, path: &[&str], new: Value) -> Result<()> {
    let (last, parents) = path
        .split_last()
        .ok_or_else(|| Error::Failed("an empty configuration path".to_owned()))?;
    let Value::Mapping(mapping) = at(root, parents)? else {
        return Err(Error::Failed(format!(
            "test.yml's {} isn't a mapping",
            parents.join(".")
        )));
    };
    // What it replaces, if anything, is test.yml's, and goes.
    mapping.insert(Value::from(*last), new);
    Ok(())
}

/// Fails unless the Redis cluster says it is formed (`CLUSTER INFO`'s `cluster_state:ok`), which
/// the node `redis-2` creates once all three run (testbox/pod.yaml `REDIS_CLUSTER_CREATOR`).
fn redis_cluster_formed() -> Result<()> {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    let address = REDIS_CLUSTER_NODE
        .to_socket_addrs()
        .describe(REDIS_CLUSTER_NODE.to_owned())?
        .next()
        .ok_or_else(|| Error::Failed(format!("{REDIS_CLUSTER_NODE} resolves to nothing")))?;
    let mut node = TcpStream::connect_timeout(&address, REDIS_PATIENCE)
        .describe(REDIS_CLUSTER_NODE.to_owned())?;
    node.set_read_timeout(Some(REDIS_PATIENCE))
        .describe(REDIS_CLUSTER_NODE.to_owned())?;
    // An inline command; the answer is a bulk string of `key:value` lines.
    node.write_all(b"CLUSTER INFO\r\n")
        .describe(REDIS_CLUSTER_NODE.to_owned())?;
    let mut answer = Vec::new();
    let mut chunk = [0; 4096];
    while !String::from_utf8_lossy(&answer).contains("cluster_state:") {
        let read = node
            .read(&mut chunk)
            .describe(REDIS_CLUSTER_NODE.to_owned())?;
        if read == 0 {
            break;
        }
        answer.extend_from_slice(&chunk[..read]);
    }
    let answer = String::from_utf8_lossy(&answer);
    if answer.lines().any(|line| line.trim() == "cluster_state:ok") {
        Ok(())
    } else {
        Err(Error::Failed(format!(
            "the Redis cluster isn't formed yet: {}",
            answer
                .lines()
                .find(|line| line.starts_with("cluster_state:"))
                .unwrap_or(answer.trim())
        )))
    }
}

/// LocalStack takes an unsigned path-style PUT; 409 means the bucket exists already.
async fn ensure_prekey_bucket(http: &reqwest::Client) -> Result<()> {
    let url = format!("{}/{}", PREKEY_BUCKET.endpoint, PREKEY_BUCKET.name);
    let response = http.put(&url).send().await.describe(format!("PUT {url}"))?;
    match response.status() {
        reqwest::StatusCode::OK | reqwest::StatusCode::CONFLICT => Ok(()),
        status => Err(Error::Failed(format!("PUT {url}: {status}"))),
    }
}

/// One DynamoDB call. DynamoDB Local runs with `-sharedDb` (testbox/dynamodb/Containerfile), so any
/// credentials see the server's database, and it doesn't check the signature.
async fn dynamodb<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    operation: &str,
    body: &impl Serialize,
) -> Result<T> {
    let response = http
        .post(DYNAMODB_ENDPOINT)
        .header("Content-Type", "application/x-amz-json-1.0")
        .header("X-Amz-Target", format!("DynamoDB_20120810.{operation}"))
        .header(
            "Authorization",
            "AWS4-HMAC-SHA256 Credential=test/20260101/local/dynamodb/aws4_request, \
             SignedHeaders=host, Signature=0",
        )
        .json(body)
        .send()
        .await
        .describe(format!("DynamoDB {operation}"))?;
    let status = response.status();
    let text = response
        .text()
        .await
        .describe(format!("DynamoDB {operation}"))?;
    if !status.is_success() {
        return Err(Error::Failed(format!(
            "DynamoDB {operation}: {status}: {text}"
        )));
    }
    serde_json::from_str(&text).describe(format!("DynamoDB {operation}: {text}"))
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct ListTables {
    limit: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct TableNames {
    table_names: Vec<String>,
}

async fn dynamodb_has_tables(http: &reqwest::Client) -> Result<bool> {
    let tables: TableNames = dynamodb(http, "ListTables", &ListTables { limit: 1 }).await?;
    Ok(!tables.table_names.is_empty())
}

/// Items written in one request (`BatchWriteItem`), by table.
#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct BatchWriteItem {
    request_items: BTreeMap<&'static str, Vec<WriteRequest>>,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct WriteRequest {
    put_request: PutRequest,
}

#[derive(Serialize)]
#[serde(rename_all = "PascalCase")]
struct PutRequest {
    item: RemoteConfigItem,
}

/// An item of the server's RemoteConfigs table (RemoteConfigs.java: name `N`, percentage `P`,
/// value `V`).
#[derive(Serialize)]
struct RemoteConfigItem {
    #[serde(rename = "N")]
    name: AttributeValue,
    #[serde(rename = "P")]
    percentage: AttributeValue,
    #[serde(rename = "V")]
    value: AttributeValue,
}

/// DynamoDB's typed value: `{"S": "…"}` or `{"N": "…"}`.
#[derive(Serialize)]
enum AttributeValue {
    S(String),
    N(String),
}

/// Desktop takes libsignal-net's port from its `serverUrl`, which carries its REST requests too
/// (ts/textsecure/preconnect.preload.ts `localTestServer`), so its libsignal-net talks to Jetty,
/// where gRPC goes unanswered (its upload form: "unexpected gRPC status: Internal"). A request goes
/// over the websocket instead when remote config gives its `grpc.` key the value "ws" (libsignal
/// rust/bridge/shared/types/src/net/remote_config.rs); Desktop passes on its
/// `desktop.libsignalNet.` keys (ts/LibsignalNetRemoteConfig.preload.ts). Android reaches the
/// omnibus server and keeps gRPC. Not test.yml's `globalConfig`: those reach clients as `global.`
/// keys (RemoteConfigsManager.java).
async fn write_remote_config(http: &reqwest::Client) -> Result<()> {
    let items = DesktopGrpcRequest::VARIANTS
        .iter()
        .map(|request| WriteRequest {
            put_request: PutRequest {
                item: RemoteConfigItem {
                    name: AttributeValue::S(request.remote_config_key()),
                    percentage: AttributeValue::N("100".to_owned()),
                    value: AttributeValue::S("ws".to_owned()),
                },
            },
        })
        .collect();
    let batch = BatchWriteItem {
        request_items: BTreeMap::from([(REMOTE_CONFIG_TABLE, items)]),
    };
    // At most 25 items a batch (DynamoDB's limit); DynamoDB Local leaves none unprocessed.
    dynamodb::<serde::de::IgnoredAny>(http, "BatchWriteItem", &batch)
        .await
        .map(drop)
}

/// Signal-Server, its log passed on; the remote config written once it has started (its tables
/// exist then); SIGTERM passed on to it.
async fn run_server(http: &reqwest::Client) -> Result<()> {
    let classpath = testbox_tools::env_var("SIGNAL_CLASSPATH")?;
    // foundationDb.serviceContainerNamePrefix (LocalFoundationDbDatabaseFactory.java:29): the
    // database is the `foundationdb0` service
    // (ServiceContainerFoundationDbDatabaseLifecycleManager, `docker:docker@foundationdb0:4500`).
    let (mut server, heard) = spawn_logged(
        Command::new("java")
            .arg(format!(
                "-Dsecrets.bundle.filename={}",
                KeyFile::Secrets.path().display()
            ))
            .arg("-DfoundationDb.serviceContainerNamePrefix=foundationdb")
            .args(["-cp", &classpath, SERVER_MAIN, "server", SERVER_CONFIG]),
    )?;
    tokio::select! {
        started = wait_for(heard, &[testbox_tools::JETTY_STARTED], SERVER_PATIENCE) => started?,
        supervised = supervise(&mut server, "Signal-Server") => return supervised,
    }
    write_remote_config(http).await?;
    eprintln!("{}", testbox_tools::SERVER_READY);
    supervise(&mut server, "Signal-Server").await
}
