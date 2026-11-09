//! CDN3's TLS: tus-server runs under `wrangler dev`, whose HTTPS is workerd's, which writes no
//! key log. So wrangler serves plain HTTP on the loopback ([`keys::CDN3_UPSTREAM_PORT`]) and
//! this front takes the TLS on [`keys::CDN3_PORT`] with the keys' certificate, copying the
//! bytes both ways; with `--capture`, its sessions' secrets go to `SSLKEYLOGFILE`
//! (`testbox_tools::tls`), like the other servers'. It starts wrangler itself, from its
//! arguments after `--`, and ends with it.

// Every outcome is handled: a discarded `Result` would hide a failure.
#![deny(clippy::let_underscore_must_use, clippy::let_underscore_untyped)]

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use testbox_tools::process::{spawn_logged, supervise};
use testbox_tools::{keys, tls};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

const USAGE: &str = "front -- <the server's command and its arguments>";

/// What keeps the front from serving.
#[derive(Debug, thiserror::Error)]
enum FrontError {
    #[error("usage: {USAGE}")]
    Usage,
    #[error("no command after --")]
    NoCommand,
    #[error("building a runtime")]
    Runtime(#[source] std::io::Error),
    #[error("listening on {address}")]
    Listen {
        address: SocketAddr,
        #[source]
        source: std::io::Error,
    },
}

/// What ends one client's connection with a failure.
#[derive(Debug, thiserror::Error)]
enum RelayError {
    #[error("setting TCP_NODELAY on the {side}'s connection")]
    NoDelay {
        side: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("TLS with the client")]
    Handshake(#[source] std::io::Error),
    #[error("connecting to {address}")]
    Connect {
        address: SocketAddr,
        #[source]
        source: std::io::Error,
    },
    #[error("relaying")]
    Relay(#[source] std::io::Error),
}

fn main() -> std::process::ExitCode {
    let outcome = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|source| FrontError::Runtime(source).into())
        .and_then(|runtime| runtime.block_on(run()));
    testbox_tools::exit("front", outcome)
}

async fn run() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut arguments = std::env::args().skip(1);
    let separator = arguments.next();
    if separator.as_deref() != Some("--") {
        return Err(FrontError::Usage.into());
    }
    let program = arguments.next().ok_or(FrontError::NoCommand)?;
    let mut command = tokio::process::Command::new(program);
    command.args(arguments);

    tls::install_provider()?;
    // Clients offer HTTP/1.1 to the CDNs, and wrangler speaks nothing else.
    let acceptor = TlsAcceptor::from(Arc::new(tls::server_config(&["http/1.1"])?));
    tls::say_where_secrets_go("front");
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, keys::CDN3_PORT));
    let listener = TcpListener::bind(address)
        .await
        .map_err(|source| FrontError::Listen { address, source })?;
    let (mut server, _heard) = spawn_logged(&mut command)?;
    eprintln!("{} on {}", testbox_tools::FRONT_READY, keys::CDN3_PORT);

    let accepting = async {
        loop {
            let (client, peer) = match listener.accept().await {
                Ok(accepted) => accepted,
                Err(error) => {
                    eprintln!("front: accepting: {error}");
                    continue;
                }
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Err(error) = relay(acceptor, client).await {
                    eprintln!("front: {peer}: {}", testbox_tools::Report(&error));
                }
            });
        }
    };
    tokio::select! {
        supervised = supervise(&mut server, "wrangler") => Ok(supervised?),
        () = accepting => Ok(()),
    }
}

/// One client's connection: its TLS ended here, its bytes copied to wrangler and back until
/// either side closes.
async fn relay(acceptor: TlsAcceptor, client: TcpStream) -> Result<(), RelayError> {
    let no_delay = |side| move |source| RelayError::NoDelay { side, source };
    client.set_nodelay(true).map_err(no_delay("client"))?;
    let mut client = acceptor
        .accept(client)
        .await
        .map_err(RelayError::Handshake)?;
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, keys::CDN3_UPSTREAM_PORT));
    let mut upstream = TcpStream::connect(address)
        .await
        .map_err(|source| RelayError::Connect { address, source })?;
    upstream.set_nodelay(true).map_err(no_delay("upstream"))?;
    match tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
        Ok(_bytes) => Ok(()),
        // A client that drops the connection without TLS's goodbye, as browsers' stacks do.
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(()),
        Err(error) => Err(RelayError::Relay(error)),
    }
}
