//! The capture's TCP connections: tcpdump's frames (Ethernet over the loopback, IPv4 or IPv6),
//! grouped by their endpoints and each side's bytes put back in sequence order, with when each
//! run of them came. Only what the loopback does happens here: segments in order, now and then
//! one again (a retransmission, skipped) or one early (held until its turn). A connection is
//! the client's, who sent the SYN, to the server, who answered it.

use std::collections::{BTreeMap, HashMap};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// One side's bytes of a connection, as sent: each run with when it came.
#[derive(Default)]
pub(super) struct Side {
    pub(super) chunks: Vec<(u64, Vec<u8>)>,
    /// When this side closed (FIN or RST), if it did.
    pub(super) closed: Option<u64>,
    /// The next sequence number expected, relative to the first byte's.
    next: Option<u32>,
    /// Segments that came before their turn, by their relative sequence number.
    early: BTreeMap<u32, (u64, Vec<u8>)>,
    /// What the SYN's sequence number was: byte 0 is the one after it.
    initial: Option<u32>,
}

/// A connection between `client` and `server`, in the order of its SYN.
pub(super) struct Connection {
    pub(super) client: SocketAddr,
    pub(super) server: SocketAddr,
    /// When the SYN came.
    pub(super) opened: u64,
    pub(super) sent: Side,
    pub(super) received: Side,
}

/// One TCP segment as a frame carried it.
struct Segment<'a> {
    source: SocketAddr,
    destination: SocketAddr,
    sequence: u32,
    syn: bool,
    fin: bool,
    rst: bool,
    ack: bool,
    payload: &'a [u8],
}

const ETHERNET_HEADER: usize = 14;
const TCP: u8 = 6;

/// The segment in an Ethernet frame, if it carries one over IPv4 or IPv6.
fn segment(frame: &[u8]) -> Option<Segment<'_>> {
    let ethertype = frame.get(12..14)?;
    let (source, destination, transport, length) = match ethertype {
        [0x08, 0x00] => {
            let ip = frame.get(ETHERNET_HEADER..ETHERNET_HEADER + 20)?;
            if ip[0] >> 4 != 4 || ip[9] != TCP {
                return None;
            }
            let header = usize::from(ip[0] & 0x0F) * 4;
            let total = usize::from(u16::from_be_bytes([ip[2], ip[3]]));
            (
                IpAddr::V4(Ipv4Addr::new(ip[12], ip[13], ip[14], ip[15])),
                IpAddr::V4(Ipv4Addr::new(ip[16], ip[17], ip[18], ip[19])),
                ETHERNET_HEADER + header,
                total.checked_sub(header)?,
            )
        }
        [0x86, 0xDD] => {
            let ip = frame.get(ETHERNET_HEADER..ETHERNET_HEADER + 40)?;
            if ip[6] != TCP {
                return None;
            }
            let address = |at: usize| {
                let mut octets = [0; 16];
                octets.copy_from_slice(&ip[at..at + 16]);
                IpAddr::V6(Ipv6Addr::from(octets))
            };
            (
                address(8),
                address(24),
                ETHERNET_HEADER + 40,
                usize::from(u16::from_be_bytes([ip[4], ip[5]])),
            )
        }
        _ => return None,
    };
    let tcp = frame.get(transport..transport + 20)?;
    let data_offset = usize::from(tcp[12] >> 4) * 4;
    let payload =
        frame.get(transport + data_offset..transport + length.min(frame.len() - transport))?;
    let flags = tcp[13];
    Some(Segment {
        source: SocketAddr::new(source, u16::from_be_bytes([tcp[0], tcp[1]])),
        destination: SocketAddr::new(destination, u16::from_be_bytes([tcp[2], tcp[3]])),
        sequence: u32::from_be_bytes([tcp[4], tcp[5], tcp[6], tcp[7]]),
        syn: flags & 0x02 != 0,
        fin: flags & 0x01 != 0,
        rst: flags & 0x04 != 0,
        ack: flags & 0x10 != 0,
        payload,
    })
}

impl Side {
    /// A segment of this side's, in sequence: its bytes added in their turn, a retransmitted
    /// part skipped, an early one held.
    fn take(&mut self, at: u64, segment: &Segment<'_>) {
        if segment.syn {
            self.initial = Some(segment.sequence);
            self.next = Some(0);
        }
        let Some(initial) = self.initial else {
            // A connection the capture saw from the middle: nothing of it is in order.
            return;
        };
        let relative = segment.sequence.wrapping_sub(initial).wrapping_sub(1);
        if !segment.payload.is_empty() {
            self.early
                .entry(relative)
                .or_insert_with(|| (at, segment.payload.to_vec()));
            self.drain();
        }
        if segment.fin || segment.rst {
            // A FIN after its data, which may still be early; RST at once.
            if segment.rst || self.early.is_empty() {
                self.closed.get_or_insert(at);
            } else {
                self.early
                    .entry(relative.wrapping_add(u32::MAX / 2))
                    .or_insert_with(|| (at, Vec::new()));
            }
        }
    }

    /// Delivers what is in order from the held segments.
    fn drain(&mut self) {
        let Some(mut next) = self.next else {
            return;
        };
        while let Some((&start, _)) = self.early.first_key_value() {
            if start.wrapping_sub(next) < u32::MAX / 4 && start != next {
                // Still a gap before it.
                break;
            }
            let (_, (at, bytes)) = self.early.pop_first().expect("the first entry");
            let skip = next.wrapping_sub(start) as usize;
            if skip < bytes.len() {
                let fresh = &bytes[skip..];
                next = next.wrapping_add(fresh.len() as u32);
                self.chunks.push((at, fresh.to_vec()));
            }
        }
        self.next = Some(next);
        // A FIN that was held behind its data.
        if let Some((&start, &(at, ref bytes))) = self.early.first_key_value()
            && bytes.is_empty()
            && start.wrapping_sub(next) >= u32::MAX / 4
        {
            self.closed.get_or_insert(at);
            self.early.pop_first();
        }
    }
}

/// The connections to `servers`' ports in `frames`, each with its bytes in order; in the order
/// their SYNs came.
pub(super) fn connections(frames: &[(u64, &[u8])], servers: &[u16]) -> Vec<Connection> {
    let mut open: HashMap<(SocketAddr, SocketAddr), usize> = HashMap::new();
    let mut found: Vec<Connection> = Vec::new();
    for &(at, frame) in frames {
        let Some(segment) = segment(frame) else {
            continue;
        };
        if segment.syn && !segment.ack {
            if !servers.contains(&segment.destination.port()) {
                continue;
            }
            // A new connection from the same ports as an old one replaces it.
            open.insert((segment.source, segment.destination), found.len());
            found.push(Connection {
                client: segment.source,
                server: segment.destination,
                opened: at,
                sent: Side::default(),
                received: Side::default(),
            });
        }
        let (index, from_client) =
            if let Some(&index) = open.get(&(segment.source, segment.destination)) {
                (index, true)
            } else if let Some(&index) = open.get(&(segment.destination, segment.source)) {
                (index, false)
            } else {
                continue;
            };
        let connection = &mut found[index];
        if from_client {
            connection.sent.take(at, &segment);
        } else {
            connection.received.take(at, &segment);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::connections;

    /// An IPv4 Ethernet frame with a TCP segment: `flags` as the TCP header has them.
    fn frame(
        from: SocketAddr,
        to: SocketAddr,
        sequence: u32,
        flags: u8,
        payload: &[u8],
    ) -> Vec<u8> {
        let (SocketAddr::V4(from), SocketAddr::V4(to)) = (from, to) else {
            panic!("IPv4")
        };
        let mut out = vec![0; 12];
        out.extend_from_slice(&[0x08, 0x00]);
        let total = 20 + 20 + payload.len();
        out.extend_from_slice(&[
            0x45,
            0,
            (total >> 8) as u8,
            total as u8,
            0,
            0,
            0x40,
            0,
            64,
            6,
            0,
            0,
        ]);
        out.extend_from_slice(&from.ip().octets());
        out.extend_from_slice(&to.ip().octets());
        out.extend_from_slice(&from.port().to_be_bytes());
        out.extend_from_slice(&to.port().to_be_bytes());
        out.extend_from_slice(&sequence.to_be_bytes());
        out.extend_from_slice(&[0, 0, 0, 0, 0x50, flags, 0xFF, 0xFF, 0, 0, 0, 0]);
        out.extend_from_slice(payload);
        out
    }

    const SYN: u8 = 0x02;
    const ACK: u8 = 0x10;
    const FIN: u8 = 0x01;
    const PSH: u8 = 0x08;

    #[test]
    fn a_connection_in_order_out_of_order_and_retransmitted() {
        let client: SocketAddr = "127.0.0.1:40000".parse().unwrap();
        let server: SocketAddr = "127.0.0.1:8444".parse().unwrap();
        let frames = [
            (1, frame(client, server, 1000, SYN, b"")),
            (2, frame(server, client, 5000, SYN | ACK, b"")),
            (3, frame(client, server, 1001, ACK, b"")),
            (4, frame(client, server, 1001, ACK | PSH, b"hello ")),
            // The second run before the first one again, then the third early.
            (5, frame(client, server, 1001, ACK | PSH, b"hello ")),
            (6, frame(client, server, 1013, ACK | PSH, b"!")),
            (7, frame(client, server, 1007, ACK | PSH, b"server")),
            (8, frame(server, client, 5001, ACK | PSH, b"hi")),
            (9, frame(client, server, 1014, ACK | FIN, b"")),
            (10, frame(server, client, 5003, ACK | FIN, b"")),
            // Another connection, to a port that isn't a server's.
            (
                11,
                frame(client, "127.0.0.1:6379".parse().unwrap(), 1, SYN, b""),
            ),
        ];
        let frames: Vec<(u64, &[u8])> = frames.iter().map(|(at, f)| (*at, f.as_slice())).collect();
        let found = connections(&frames, &[8444]);
        assert_eq!(found.len(), 1);
        let connection = &found[0];
        assert_eq!(connection.opened, 1);
        assert_eq!(connection.client, client);
        let sent: Vec<u8> = connection
            .sent
            .chunks
            .iter()
            .flat_map(|(_, b)| b.clone())
            .collect();
        assert_eq!(sent, b"hello server!");
        assert_eq!(
            connection
                .sent
                .chunks
                .iter()
                .map(|(at, _)| *at)
                .collect::<Vec<_>>(),
            [4, 7, 6]
        );
        assert_eq!(connection.received.chunks, [(8, b"hi".to_vec())]);
        assert_eq!(connection.sent.closed, Some(9));
        assert_eq!(connection.received.closed, Some(10));
    }
}
