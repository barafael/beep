//! `--capture`'s relay: what the clients say to the servers over TLS, in plaintext, for comparing
//! Signal's own clients with b33p (xtask/src/wire). `tap redirect`, in an init container with
//! NET_ADMIN, sends every connection to one of [`keys::TAPPED_PORTS`] on the pod's loopback to
//! `tap` instead ([`keys::tap_port`]), but its own; `tap` takes each, opens one to the server the
//! client meant, and copies the bytes between the two, writing them down
//! (`testbox_tools::tap`).
//!
//! It shows the client the keys' certificate, the one the servers show, and the protocol the
//! server chose from those the client offered (ALPN): h2 stays h2, and HTTP/2's extended CONNECT
//! and gRPC go through as they are, as the tap reads none of it.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::io::Write as _;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::server::Acceptor;
use rustls::{ClientConfig, RootCertStore, ServerConfig};
use testbox_tools::keys::{self, KeyFile};
use testbox_tools::tap::{Connection, Direction, Hello, Record, file_names};
use testbox_tools::{Context, Describe, Error, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpSocket, TcpStream};
use tokio::sync::mpsc;
use tokio_rustls::{LazyConfigAcceptor, TlsConnector};

/// What one read takes at most.
const CHUNK: usize = 64 * 1024;

fn main() -> std::process::ExitCode {
    let outcome = match testbox_tools::argument("tap [redirect]") {
        Ok(Some(argument)) if argument == "redirect" => redirect(),
        Ok(Some(argument)) => Err(Error::Failed(format!(
            "usage: tap [redirect], not {argument}"
        ))),
        Ok(None) => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .describe("a runtime")
            .and_then(|runtime| runtime.block_on(relay())),
        Err(error) => Err(error),
    };
    testbox_tools::exit("tap", outcome)
}

/// The pod's redirect to the tap: nftables' NAT on the way out, IPv4 and IPv6, for the
/// connections to the tapped ports; not for the tap's own, from [`keys::TAP_SOURCE`].
fn redirect() -> Result<()> {
    let ports: Vec<String> = keys::TAPPED_PORTS.iter().map(u16::to_string).collect();
    let map: Vec<String> = keys::TAPPED_PORTS
        .iter()
        .map(|&port| format!("{port} : {}", keys::tap_port(port)))
        .collect();
    let source = Ipv4Addr::from(keys::TAP_SOURCE);
    let rules = format!(
        "table inet tap {{\n\
         \tchain output {{\n\
         \t\ttype nat hook output priority -100; policy accept;\n\
         \t\tip saddr {source} return\n\
         \t\ttcp dport {{ {} }} redirect to :tcp dport map {{ {} }}\n\
         \t}}\n\
         }}\n",
        ports.join(", "),
        map.join(", "),
    );
    let mut nft = std::process::Command::new("nft")
        .args(["-f", "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("nft")?;
    nft.stdin
        .take()
        .ok_or_else(|| Error::Failed("nft has no stdin".to_owned()))?
        .write_all(rules.as_bytes())
        .context("the rules to nft")?;
    let status = nft.wait().context("nft")?;
    if !status.success() {
        return Err(Error::Failed(format!("nft {status}, the rules:\n{rules}")));
    }
    eprintln!("tap: connections to {} go to the tap", ports.join(", "));
    Ok(())
}

async fn relay() -> Result<()> {
    rustls::crypto::ring::default_provider()
        .install_default()
        .map_err(|_installed| Error::Failed("a TLS provider is installed already".to_owned()))?;
    let tls = Arc::new(Tls::load()?);
    let directory = PathBuf::from(keys::TAP_DIR);
    let next = Arc::new(AtomicU64::new(0));
    let mut listeners = Vec::new();
    for port in keys::TAPPED_PORTS {
        let tapped = keys::tap_port(port);
        listeners.push((
            port,
            TcpListener::bind((Ipv4Addr::LOCALHOST, tapped))
                .await
                .context(format!("127.0.0.1:{tapped}"))?,
        ));
        // Those clients that resolve localhost to ::1 first.
        match TcpListener::bind((Ipv6Addr::LOCALHOST, tapped)).await {
            Ok(listener) => listeners.push((port, listener)),
            Err(error) => eprintln!("tap: not on [::1]:{tapped}: {error}"),
        }
    }
    eprintln!(
        "{} {}",
        testbox_tools::TAP_READY,
        keys::TAPPED_PORTS.map(|port| port.to_string()).join(", ")
    );
    let mut accepting = tokio::task::JoinSet::new();
    for (port, listener) in listeners {
        let tls = tls.clone();
        let directory = directory.clone();
        let next = next.clone();
        accepting.spawn(async move {
            loop {
                let (client, peer) = match listener.accept().await {
                    Ok(accepted) => accepted,
                    Err(error) => {
                        eprintln!("tap: accepting on {port}: {error}");
                        continue;
                    }
                };
                let n = next.fetch_add(1, Ordering::Relaxed);
                let tls = tls.clone();
                let directory = directory.clone();
                tokio::spawn(async move {
                    if let Err(error) = connection(&tls, &directory, n, port, client, peer).await {
                        eprintln!("tap: connection {n} to {port}: {error}");
                    }
                });
            }
        });
    }
    // The accepting loops end only with the runtime, or when one panicked.
    match accepting.join_next().await {
        Some(Err(panicked)) => Err(panicked).describe("an accepting loop"),
        Some(Ok(())) | None => Ok(()),
    }
}

/// The keys the tap shows clients, and trusts servers by.
struct Tls {
    certificates: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
    roots: Arc<RootCertStore>,
}

impl Tls {
    fn load() -> Result<Self> {
        let certificates = CertificateDer::pem_file_iter(KeyFile::TlsCertificate.path())
            .describe("the keys' certificate")?
            .collect::<std::result::Result<Vec<_>, _>>()
            .describe("the keys' certificate")?;
        let key = PrivateKeyDer::from_pem_file(KeyFile::TlsKey.path()).describe("the keys' key")?;
        let mut roots = RootCertStore::empty();
        for ca in CertificateDer::pem_file_iter(KeyFile::TlsCa.path()).describe("the keys' CA")? {
            roots
                .add(ca.describe("the keys' CA")?)
                .describe("the keys' CA")?;
        }
        Ok(Self {
            certificates,
            key,
            roots: Arc::new(roots),
        })
    }
}

/// One client's connection, relayed to the server it meant on `port` and written down as
/// connection `n`.
async fn connection(
    tls: &Tls,
    directory: &Path,
    n: u64,
    port: u16,
    client: TcpStream,
    peer: SocketAddr,
) -> Result<()> {
    let (bytes_name, meta_name) = file_names(n, port);
    let meta_path = directory.join(meta_name);
    let mut meta = Connection {
        n,
        port,
        peer: peer.to_string(),
        opened: now(),
        hello: Hello::default(),
        alpn: None,
        upstream_version: None,
        upstream_suite: None,
        failed: None,
    };
    let relayed = handshakes(tls, port, client, &mut meta).await;
    let (client, server) = match relayed {
        Ok(streams) => streams,
        Err(error) => {
            meta.failed = Some(error.to_string());
            write_meta(&meta_path, &meta)?;
            return Err(error);
        }
    };
    write_meta(&meta_path, &meta)?;
    let file = std::fs::File::create(directory.join(&bytes_name)).context(bytes_name.clone())?;
    let (records, written) = mpsc::unbounded_channel();
    let writer = tokio::task::spawn_blocking(move || write_records(file, written));
    let (client_read, client_write) = tokio::io::split(client);
    let (server_read, server_write) = tokio::io::split(server);
    let sent = copy(
        client_read,
        server_write,
        Direction::Sent,
        Direction::ClientClosed,
        records.clone(),
    );
    let received = copy(
        server_read,
        client_write,
        Direction::Received,
        Direction::ServerClosed,
        records,
    );
    let (sent, received) = tokio::join!(sent, received);
    writer
        .await
        .describe("the record writer")?
        .context(bytes_name)?;
    sent.and(received)
}

/// The client's hello read, the server's connection made with what it offered, and the client's
/// handshake finished with what the server chose.
async fn handshakes(
    tls: &Tls,
    port: u16,
    client: TcpStream,
    meta: &mut Connection,
) -> Result<(
    tokio_rustls::server::TlsStream<TcpStream>,
    tokio_rustls::client::TlsStream<TcpStream>,
)> {
    let start = LazyConfigAcceptor::new(Acceptor::default(), client)
        .await
        .describe("the client's hello")?;
    let hello = start.client_hello();
    let offered: Vec<Vec<u8>> = hello
        .alpn()
        .map(|protocols| protocols.map(<[u8]>::to_vec).collect())
        .unwrap_or_default();
    meta.hello = Hello {
        server_name: hello.server_name().map(str::to_owned),
        alpn: offered
            .iter()
            .map(|protocol| String::from_utf8_lossy(protocol).into_owned())
            .collect(),
        cipher_suites: hello
            .cipher_suites()
            .iter()
            .map(|suite| format!("{suite:?}"))
            .collect(),
        signature_schemes: hello
            .signature_schemes()
            .iter()
            .map(|scheme| format!("{scheme:?}"))
            .collect(),
    };
    let name = ServerName::try_from(
        meta.hello
            .server_name
            .clone()
            .unwrap_or_else(|| "localhost".to_owned()),
    )
    .describe("the server's name")?;

    let mut upstream = ClientConfig::builder()
        .with_root_certificates(tls.roots.clone())
        .with_no_client_auth();
    upstream.alpn_protocols = offered;
    let socket = TcpSocket::new_v4().context("a socket")?;
    socket
        .bind(SocketAddr::new(IpAddr::from(keys::TAP_SOURCE), 0))
        .context("binding to the tap's source address")?;
    let tcp = socket
        .connect(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .await
        .context(format!("127.0.0.1:{port}"))?;
    tcp.set_nodelay(true).context("TCP_NODELAY")?;
    let server = TlsConnector::from(Arc::new(upstream))
        .connect(name, tcp)
        .await
        .context(format!("TLS to {port}"))?;
    let (_, session) = server.get_ref();
    let chosen = session.alpn_protocol().map(<[u8]>::to_vec);
    meta.alpn = chosen
        .as_deref()
        .map(|protocol| String::from_utf8_lossy(protocol).into_owned());
    meta.upstream_version = session
        .protocol_version()
        .map(|version| format!("{version:?}"));
    meta.upstream_suite = session
        .negotiated_cipher_suite()
        .map(|suite| format!("{:?}", suite.suite()));

    let mut shown = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(tls.certificates.clone(), tls.key.clone_key())
        .describe("the keys' certificate")?;
    shown.alpn_protocols = chosen.into_iter().collect();
    let client = start
        .into_stream(Arc::new(shown))
        .await
        .context("TLS with the client")?;
    client
        .get_ref()
        .0
        .set_nodelay(true)
        .context("TCP_NODELAY")?;
    Ok((client, server))
}

/// Copies from `from` to `to` until `from` ends, each read written down as `direction`, then
/// closes `to` and writes down `closed`.
async fn copy(
    mut from: impl AsyncRead + Unpin,
    mut to: impl AsyncWrite + Unpin,
    direction: Direction,
    closed: Direction,
    records: mpsc::UnboundedSender<Record>,
) -> Result<()> {
    let mut buffer = vec![0; CHUNK];
    let outcome = loop {
        let read = match from.read(&mut buffer).await {
            Ok(0) => break Ok(()),
            Ok(read) => read,
            // A side that goes without TLS's close_notify, as many do, ends it too.
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => break Ok(()),
            Err(error) => break Err(error).context(format!("reading {direction:?}")),
        };
        let bytes = buffer[..read].to_vec();
        if let Err(error) = to.write_all(&bytes).await {
            break Err(error).context(format!("writing {direction:?}"));
        }
        // The writer goes only once both copies have ended.
        records
            .send(Record {
                at: now(),
                direction,
                bytes,
            })
            .ok();
    };
    // The other side may be gone already.
    to.shutdown().await.ok();
    records
        .send(Record {
            at: now(),
            direction: closed,
            bytes: Vec::new(),
        })
        .ok();
    outcome
}

/// Writes the records as they come, each batch flushed: what came before a box stopped is in the
/// file.
fn write_records(
    file: std::fs::File,
    mut records: mpsc::UnboundedReceiver<Record>,
) -> std::io::Result<()> {
    let mut out = std::io::BufWriter::new(file);
    while let Some(record) = records.blocking_recv() {
        record.write_to(&mut out)?;
        while let Ok(record) = records.try_recv() {
            record.write_to(&mut out)?;
        }
        out.flush()?;
    }
    out.flush()
}

fn write_meta(path: &Path, meta: &Connection) -> Result<()> {
    let json = serde_json::to_string(meta).describe("a connection's JSON")?;
    testbox_tools::write(path, json)
}

/// Nanoseconds since the epoch.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
        })
}
