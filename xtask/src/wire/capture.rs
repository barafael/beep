//! The connections in a record's capture, decoded: the pcap's frames into TCP connections to
//! the servers' ports ([`super::tcp`]), each opened with the servers' key logs ([`super::tls`]),
//! into the form the rest of the decoding reads ([`testbox_tools::connections`]): what the
//! connection was, and its plaintext records in order.

use std::net::SocketAddr;
use std::path::Path;

use testbox_tools::connections::{Connection, Direction, Record};
use testbox_tools::keys::CAPTURED_PORTS;

use super::pcap::{self, PcapError};
use super::stream::Stream;
use super::tls::KeyLog;
use super::{tcp, tls};

/// Every key log in `raw`: the servers' `keys-<server>.log`, and a `keys.log` from before.
pub(super) fn key_log(raw: &Path) -> Result<KeyLog, PcapError> {
    Ok(KeyLog::parse(&String::from_utf8_lossy(&pcap::key_logs(
        raw,
    )?)))
}

/// A connection as the rest of the decoding reads it: what it was, and its plaintext records.
pub(super) type Decoded = (Connection, Vec<Record>);

/// The connections in `raw`'s capture, with their plaintext; `None` without a capture (a
/// record from before, whose `raw/` holds the relay's files instead).
pub(super) fn connections(raw: &Path) -> Result<Option<Vec<Decoded>>, PcapError> {
    let Some(bytes) = pcap::read(raw)? else {
        return Ok(None);
    };
    let frames = pcap::frames(&raw.join(pcap::CAPTURE), &bytes)?;
    let log = key_log(raw)?;
    println!(
        "wire: {} frames, the key logs' secrets for {} sessions",
        frames.len(),
        log.sessions()
    );
    let mut found = Vec::new();
    for (n, connection) in tcp::connections(&frames, &CAPTURED_PORTS)
        .into_iter()
        .enumerate()
    {
        let (sent, received) = sides(&connection);
        let decoded = tls::decode(&sent, &received, &log);
        let mut records = decoded.records;
        // Each side's close, when it came, after what it sent before.
        for (closed, direction) in [
            (connection.sent.closed, Direction::ClientClosed),
            (connection.received.closed, Direction::ServerClosed),
        ] {
            if let Some(at) = closed {
                records.push(Record {
                    at,
                    direction,
                    bytes: Vec::new(),
                });
            }
        }
        records.sort_by_key(|record| record.at);
        let failed = if sent.bytes.is_empty() {
            Some("nothing sent".to_owned())
        } else {
            decoded.failed
        };
        found.push((
            Connection {
                n: n as u64,
                port: connection.server.port(),
                peer: peer(connection.client),
                opened: connection.opened,
                hello: decoded.handshake.hello,
                alpn: decoded.handshake.alpn,
                upstream_version: decoded.handshake.version,
                upstream_suite: decoded.handshake.suite,
                failed,
            },
            records,
        ));
    }
    Ok(Some(found))
}

/// The two sides' bytes as [`Stream`]s, with when each run came.
fn sides(connection: &tcp::Connection) -> (Stream, Stream) {
    let stream = |side: &tcp::Side| {
        let mut stream = Stream::default();
        for (at, bytes) in &side.chunks {
            stream.push(*at, bytes);
        }
        stream.closed = side.closed;
        stream
    };
    (stream(&connection.sent), stream(&connection.received))
}

/// The client's address as the relay wrote it: `127.0.0.1:1234`, `[::1]:1234`.
fn peer(address: SocketAddr) -> String {
    address.to_string()
}
