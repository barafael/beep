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
use testbox_tools::{Context, Describe, Error, Result, keys, tls};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::TlsAcceptor;

fn main() -> std::process::ExitCode {
    let outcome = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .describe("a runtime")
        .and_then(|runtime| runtime.block_on(run()));
    testbox_tools::exit("front", outcome)
}

async fn run() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    let separator = arguments.next();
    if separator.as_deref() != Some("--") {
        return Err(Error::Failed(
            "usage: front -- <the server's command and its arguments>".to_owned(),
        ));
    }
    let program = arguments
        .next()
        .ok_or_else(|| Error::Failed("front: no command after --".to_owned()))?;
    let mut command = tokio::process::Command::new(program);
    command.args(arguments);

    tls::install_provider()?;
    // Clients offer HTTP/1.1 to the CDNs, and wrangler speaks nothing else.
    let acceptor = TlsAcceptor::from(Arc::new(tls::server_config(&["http/1.1"])?));
    tls::say_where_secrets_go("front");
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, keys::CDN3_PORT))
        .await
        .context(format!("127.0.0.1:{}", keys::CDN3_PORT))?;
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
                    eprintln!("front: {peer}: {error}");
                }
            });
        }
    };
    tokio::select! {
        supervised = supervise(&mut server, "wrangler") => supervised,
        () = accepting => Ok(()),
    }
}

/// One client's connection: its TLS ended here, its bytes copied to wrangler and back until
/// either side closes.
async fn relay(acceptor: TlsAcceptor, client: TcpStream) -> Result<()> {
    client.set_nodelay(true).context("TCP_NODELAY")?;
    let mut client = acceptor
        .accept(client)
        .await
        .context("TLS with the client")?;
    let mut upstream = TcpStream::connect(SocketAddr::from((
        Ipv4Addr::LOCALHOST,
        keys::CDN3_UPSTREAM_PORT,
    )))
    .await
    .context(format!("127.0.0.1:{}", keys::CDN3_UPSTREAM_PORT))?;
    upstream.set_nodelay(true).context("TCP_NODELAY")?;
    match tokio::io::copy_bidirectional(&mut client, &mut upstream).await {
        Ok(_bytes) => Ok(()),
        // A client that drops the connection without TLS's goodbye, as browsers' stacks do.
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(()),
        Err(error) => Err(error).context("relaying"),
    }
}
