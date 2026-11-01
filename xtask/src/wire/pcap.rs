//! The packet capture and the TLS secrets put together, for Wireshark. With `--capture`, tcpdump
//! writes the pod's loopback down as it was on the wire (testbox/capture, `capture.pcap`), and
//! each server writes its TLS sessions' secrets down (`SSLKEYLOGFILE`, `keys-<server>.log`).
//! Neither alone reads. This module writes them as one pcapng, `capture.pcapng`, whose
//! Decryption Secrets Block carries the key logs: Wireshark decrypts it on opening, with
//! nothing to configure, the way `editcap --inject-secrets tls,keys.log` would with Wireshark
//! on the host. [`super::capture`] decodes the same two into the comparison's connections.
//!
//! The pcap is read as tcpdump leaves it: a record still being written, at the end, is left out.
//! pcapng as its specification (draft-ietf-opsawg-pcapng): a Section Header Block, an Interface
//! Description Block for the one interface, the secrets, then an Enhanced Packet Block a packet.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use crate::error::{Context, Error, Result};

/// What tcpdump writes in the capture's directory (testbox/capture/pod.yaml).
pub(super) const CAPTURE: &str = "capture.pcap";
/// The two together, in the record's `wire/`.
pub(super) const PCAPNG: &str = "capture.pcapng";

/// `raw`'s [`CAPTURE`], whole; `None` without one (a record from before there was one).
pub(super) fn read(raw: &Path) -> Result<Option<Vec<u8>>> {
    let capture = raw.join(CAPTURE);
    match std::fs::read(&capture) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).context(capture.display().to_string()),
    }
}

/// The frames in a pcap, each with when it came (nanoseconds since the epoch).
pub(super) fn frames(pcap: &[u8]) -> Result<Vec<(u64, &[u8])>> {
    let header = header(pcap)?;
    Ok(packets(&header, pcap)
        .into_iter()
        .map(|(at, _original, bytes)| (at, bytes))
        .collect())
}

/// Every key log in `raw` as one: the servers' `keys-<server>.log`, and a `keys.log` from
/// before, in name order.
fn key_logs(raw: &Path) -> Result<Vec<u8>> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(raw)
        .context(raw.display().to_string())?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            name.starts_with("keys") && name.ends_with(".log")
        })
        .collect();
    names.sort();
    let mut keys = Vec::new();
    for path in names {
        keys.extend_from_slice(&std::fs::read(&path).context(path.display().to_string())?);
        if keys.last().is_some_and(|&byte| byte != b'\n') {
            keys.push(b'\n');
        }
    }
    Ok(keys)
}

/// Writes `wire`'s [`PCAPNG`] from `raw`'s [`CAPTURE`] and its key logs: how many packets it
/// holds and whether the secrets are in it; nothing, without a capture.
pub(super) fn assemble(raw: &Path, wire: &Path) -> Result<Option<Assembled>> {
    let capture = raw.join(CAPTURE);
    let Some(pcap) = read(raw)? else {
        return Ok(None);
    };
    let keys = Some(key_logs(raw)?);
    let path = wire.join(PCAPNG);
    let file = std::fs::File::create(&path).context(path.display().to_string())?;
    let mut out = BufWriter::new(file);
    let packets = write_pcapng(&mut out, &pcap, keys.as_deref()).map_err(|error| match error {
        Error::Io { what, source } => Error::Io {
            what: format!("{}: {what}", path.display()),
            source,
        },
        other => Error::Failed(format!("{}: {other}", capture.display())),
    })?;
    out.flush().context(path.display().to_string())?;
    Ok(Some(Assembled {
        packets,
        secrets: keys.is_some_and(|keys| !keys.is_empty()),
    }))
}

pub(super) struct Assembled {
    pub(super) packets: usize,
    pub(super) secrets: bool,
}

/// pcap's header, as tcpdump writes it: the magic says the byte order and whether the
/// timestamps' fraction is in microseconds or nanoseconds.
struct Header {
    swapped: bool,
    /// The fraction's digits: 6 or 9.
    resolution: u8,
    snaplen: u32,
    link_type: u16,
}

const HEADER: usize = 24;
const RECORD: usize = 16;

fn header(pcap: &[u8]) -> Result<Header> {
    if pcap.len() < HEADER {
        return Err(Error::Failed(format!(
            "a pcap header is {HEADER} bytes, this file {}",
            pcap.len()
        )));
    }
    let magic = u32::from_le_bytes([pcap[0], pcap[1], pcap[2], pcap[3]]);
    let (swapped, resolution) = match magic {
        0xA1B2_C3D4 => (false, 6),
        0xD4C3_B2A1 => (true, 6),
        0xA1B2_3C4D => (false, 9),
        0x4D3C_B2A1 => (true, 9),
        other => {
            return Err(Error::Failed(format!(
                "not a pcap: its magic is {other:#010x}"
            )));
        }
    };
    let word = |at: usize| {
        let bytes = [pcap[at], pcap[at + 1], pcap[at + 2], pcap[at + 3]];
        if swapped {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        }
    };
    Ok(Header {
        swapped,
        resolution,
        snaplen: word(16),
        // The link type's low 28 bits: the top ones say whether frames carry their FCS.
        link_type: u16::try_from(word(20) & 0x0FFF_FFFF).unwrap_or(u16::MAX),
    })
}

/// The pcap's packets, each with its timestamp in the header's resolution, its length on the
/// wire, and its captured bytes; a record cut short at the end is not one.
fn packets<'a>(header: &Header, pcap: &'a [u8]) -> Vec<(u64, u32, &'a [u8])> {
    let word = |at: usize| {
        let bytes = [pcap[at], pcap[at + 1], pcap[at + 2], pcap[at + 3]];
        if header.swapped {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        }
    };
    let scale = 10u64.pow(u32::from(header.resolution));
    let mut found = Vec::new();
    let mut at = HEADER;
    while at + RECORD <= pcap.len() {
        let seconds = u64::from(word(at));
        let fraction = u64::from(word(at + 4));
        let captured = word(at + 8) as usize;
        let original = word(at + 12);
        let start = at + RECORD;
        let Some(end) = start.checked_add(captured) else {
            break;
        };
        if end > pcap.len() {
            break;
        }
        found.push((seconds * scale + fraction, original, &pcap[start..end]));
        at = end;
    }
    found
}

/// pcapng's blocks: each a type, its total length, its body padded to 32 bits, and the total
/// length again, so that it reads backwards too.
fn block(out: &mut impl Write, kind: u32, body: &[u8]) -> std::io::Result<()> {
    let padding = (4 - body.len() % 4) % 4;
    let total = u32::try_from(12 + body.len() + padding)
        .map_err(|_| std::io::Error::other("a block over 4 GiB"))?;
    out.write_all(&kind.to_le_bytes())?;
    out.write_all(&total.to_le_bytes())?;
    out.write_all(body)?;
    out.write_all(&[0; 4][..padding])?;
    out.write_all(&total.to_le_bytes())
}

/// One option of a block: its code, its length, its value padded to 32 bits.
fn option(body: &mut Vec<u8>, code: u16, value: &[u8]) {
    body.extend_from_slice(&code.to_le_bytes());
    body.extend_from_slice(&u16::try_from(value.len()).unwrap_or(u16::MAX).to_le_bytes());
    body.extend_from_slice(value);
    body.extend_from_slice(&[0; 4][..(4 - value.len() % 4) % 4]);
}

const END_OF_OPTIONS: u16 = 0;
const SECTION_HEADER: u32 = 0x0A0D_0D0A;
const INTERFACE_DESCRIPTION: u32 = 1;
const ENHANCED_PACKET: u32 = 6;
const DECRYPTION_SECRETS: u32 = 0x0000_000A;
/// The secrets' type: a TLS key log, as `SSLKEYLOGFILE`'s.
const TLS_KEY_LOG: u32 = 0x544C_534B;

/// An I/O failure while writing `what`.
fn io(what: &'static str) -> impl Fn(std::io::Error) -> Error {
    move |source| Error::Io {
        what: what.to_owned(),
        source,
    }
}

/// How many packets it wrote.
fn write_pcapng(out: &mut impl Write, pcap: &[u8], keys: Option<&[u8]>) -> Result<usize> {
    let header = header(pcap)?;
    let mut section = Vec::new();
    section.extend_from_slice(&0x1A2B_3C4Du32.to_le_bytes());
    section.extend_from_slice(&1u16.to_le_bytes());
    section.extend_from_slice(&0u16.to_le_bytes());
    // The section's length: not known, as the specification allows.
    section.extend_from_slice(&u64::MAX.to_le_bytes());
    option(
        &mut section,
        4,
        b"b33p testbox: tcpdump on the pod's loopback, the tap's TLS secrets",
    );
    option(&mut section, END_OF_OPTIONS, &[]);
    block(out, SECTION_HEADER, &section).map_err(io("the section header"))?;

    let mut interface = Vec::new();
    interface.extend_from_slice(&header.link_type.to_le_bytes());
    interface.extend_from_slice(&0u16.to_le_bytes());
    interface.extend_from_slice(&header.snaplen.to_le_bytes());
    option(&mut interface, 2, b"lo");
    option(&mut interface, 9, &[header.resolution]);
    option(&mut interface, END_OF_OPTIONS, &[]);
    block(out, INTERFACE_DESCRIPTION, &interface).map_err(io("the interface"))?;

    if let Some(keys) = keys.filter(|keys| !keys.is_empty()) {
        let mut secrets = Vec::new();
        secrets.extend_from_slice(&TLS_KEY_LOG.to_le_bytes());
        secrets.extend_from_slice(
            &u32::try_from(keys.len())
                .map_err(|_| Error::Failed("a key log over 4 GiB".to_owned()))?
                .to_le_bytes(),
        );
        secrets.extend_from_slice(keys);
        block(out, DECRYPTION_SECRETS, &secrets).map_err(io("the secrets"))?;
    }

    let packets = packets(&header, pcap);
    for &(at, original, bytes) in &packets {
        let mut packet = Vec::with_capacity(20 + bytes.len());
        packet.extend_from_slice(&0u32.to_le_bytes());
        packet.extend_from_slice(&u32::try_from(at >> 32).unwrap_or(u32::MAX).to_le_bytes());
        packet.extend_from_slice(&(at as u32).to_le_bytes());
        packet.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_le_bytes());
        packet.extend_from_slice(&original.to_le_bytes());
        packet.extend_from_slice(bytes);
        block(out, ENHANCED_PACKET, &packet).map_err(io("a packet"))?;
    }
    Ok(packets.len())
}

#[cfg(test)]
mod tests {
    use super::{
        DECRYPTION_SECRETS, ENHANCED_PACKET, INTERFACE_DESCRIPTION, SECTION_HEADER, write_pcapng,
    };

    /// A pcap as tcpdump writes it with `--time-stamp-precision=nano` on a little-endian host:
    /// `packets`' (seconds, nanoseconds, bytes), then `tail` as a record cut short.
    fn pcap(packets: &[(u32, u32, &[u8])], tail: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&0xA1B2_3C4Du32.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&0i32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&262_144u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        for &(seconds, nanoseconds, bytes) in packets {
            out.extend_from_slice(&seconds.to_le_bytes());
            out.extend_from_slice(&nanoseconds.to_le_bytes());
            out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            out.extend_from_slice(&(bytes.len() as u32 + 4).to_le_bytes());
            out.extend_from_slice(bytes);
        }
        out.extend_from_slice(tail);
        out
    }

    /// The pcapng's blocks: each's type and body, checked to read forwards and backwards.
    fn blocks(pcapng: &[u8]) -> Vec<(u32, &[u8])> {
        let word = |at: usize| u32::from_le_bytes(pcapng[at..at + 4].try_into().unwrap());
        let mut found = Vec::new();
        let mut at = 0;
        while at < pcapng.len() {
            let kind = word(at);
            let total = word(at + 4) as usize;
            assert_eq!(total % 4, 0, "block at {at} is {total} bytes");
            assert_eq!(word(at + total - 4) as usize, total, "block at {at}");
            found.push((kind, &pcapng[at + 8..at + total - 4]));
            at += total;
        }
        found
    }

    #[test]
    fn packets_and_secrets_become_blocks() {
        let capture = pcap(
            &[
                (1_700_000_000, 5, b"abc"),
                (1_700_000_001, 999_999_999, b"defgh"),
            ],
            &[1, 2, 3],
        );
        let keys = b"CLIENT_RANDOM 00 11\n";
        let mut out = Vec::new();
        let written = write_pcapng(&mut out, &capture, Some(keys)).expect("a pcapng");
        assert_eq!(written, 2);
        let blocks = blocks(&out);
        let kinds: Vec<u32> = blocks.iter().map(|(kind, _)| *kind).collect();
        assert_eq!(
            kinds,
            [
                SECTION_HEADER,
                INTERFACE_DESCRIPTION,
                DECRYPTION_SECRETS,
                ENHANCED_PACKET,
                ENHANCED_PACKET
            ]
        );
        assert_eq!(&blocks[0].1[..4], &0x1A2B_3C4Du32.to_le_bytes());
        // The interface: Ethernet, nanoseconds.
        assert_eq!(&blocks[1].1[..2], &1u16.to_le_bytes());
        assert!(blocks[1].1.windows(5).any(|w| w == [9, 0, 1, 0, 9]));
        assert_eq!(&blocks[2].1[..4], &0x544C_534Bu32.to_le_bytes());
        assert_eq!(&blocks[2].1[8..8 + keys.len()], keys);
        let (_, packet) = blocks[4];
        let at = u64::from(u32::from_le_bytes(packet[4..8].try_into().unwrap())) << 32
            | u64::from(u32::from_le_bytes(packet[8..12].try_into().unwrap()));
        assert_eq!(at, 1_700_000_001_999_999_999);
        assert_eq!(&packet[12..16], &5u32.to_le_bytes());
        assert_eq!(&packet[16..20], &9u32.to_le_bytes());
        assert_eq!(&packet[20..25], b"defgh");
    }

    #[test]
    fn without_secrets_there_is_no_secrets_block() {
        let capture = pcap(&[(1, 0, b"x")], &[]);
        let mut out = Vec::new();
        write_pcapng(&mut out, &capture, Some(b"")).expect("a pcapng");
        let kinds: Vec<u32> = blocks(&out).iter().map(|(kind, _)| *kind).collect();
        assert_eq!(
            kinds,
            [SECTION_HEADER, INTERFACE_DESCRIPTION, ENHANCED_PACKET]
        );
    }

    #[test]
    fn microseconds_and_the_other_byte_order_read_too() {
        let mut capture = Vec::new();
        capture.extend_from_slice(&0xA1B2_C3D4u32.to_be_bytes());
        capture.extend_from_slice(&[0, 2, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0]);
        capture.extend_from_slice(&65535u32.to_be_bytes());
        capture.extend_from_slice(&1u32.to_be_bytes());
        capture.extend_from_slice(&7u32.to_be_bytes());
        capture.extend_from_slice(&3u32.to_be_bytes());
        capture.extend_from_slice(&1u32.to_be_bytes());
        capture.extend_from_slice(&1u32.to_be_bytes());
        capture.push(b'q');
        let mut out = Vec::new();
        assert_eq!(write_pcapng(&mut out, &capture, None).expect("a pcapng"), 1);
        let blocks = blocks(&out);
        assert!(blocks[1].1.windows(5).any(|w| w == [9, 0, 1, 0, 6]));
        let packet = blocks[2].1;
        assert_eq!(&packet[8..12], &7_000_003u32.to_le_bytes());
    }

    #[test]
    fn not_a_pcap_is_said() {
        let error = write_pcapng(&mut Vec::new(), b"not a pcap at all, not even close", None)
            .expect_err("no pcapng");
        assert!(error.to_string().contains("not a pcap"), "{error}");
    }
}
