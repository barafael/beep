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

/// What fails reading the capture and the key logs, or writing them down as one pcapng.
#[derive(Debug, thiserror::Error)]
pub(crate) enum PcapError {
    #[error("reading the capture {}", path.display())]
    ReadCapture {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// Shorter than pcap's header.
    #[error("{} is {len} bytes, a pcap header {HEADER}", path.display())]
    Short { path: PathBuf, len: usize },
    /// Not a magic of pcap's, in either byte order or resolution.
    #[error("{} is not a pcap: its magic is {magic:#010x}", path.display())]
    Magic { path: PathBuf, magic: u32 },
    #[error("listing {} for its key logs", path.display())]
    ListKeyLogs {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("reading the key log {}", path.display())]
    ReadKeyLog {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("creating {}", path.display())]
    Create {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// A block's total length is 32 bits.
    #[error("{block} would be a block of {len} bytes, over 4 GiB")]
    BlockTooLarge { block: &'static str, len: usize },
    /// The secrets' length is 32 bits.
    #[error("the key logs are {len} bytes, over 4 GiB")]
    KeyLogTooLarge { len: usize },
    #[error("writing {block} to {}", path.display())]
    Write {
        path: PathBuf,
        block: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("flushing {}", path.display())]
    Flush {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// What tcpdump writes in the capture's directory (testbox/capture/pod.yaml).
pub(super) const CAPTURE: &str = "capture.pcap";
/// The two together, in the record's `wire/`.
pub(super) const PCAPNG: &str = "capture.pcapng";

/// `raw`'s [`CAPTURE`], whole; `None` without one (a record from before there was one).
pub(super) fn read(raw: &Path) -> Result<Option<Vec<u8>>, PcapError> {
    let capture = raw.join(CAPTURE);
    match std::fs::read(&capture) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(PcapError::ReadCapture {
            path: capture,
            source,
        }),
    }
}

/// The frames in a pcap, `capture`'s, each with when it came (nanoseconds since the epoch).
pub(super) fn frames<'a>(
    capture: &Path,
    pcap: &'a [u8],
) -> Result<Vec<(u64, &'a [u8])>, PcapError> {
    let header = header(capture, pcap)?;
    Ok(packets(&header, pcap)
        .into_iter()
        .map(|(at, _original, bytes)| (at, bytes))
        .collect())
}

/// Every key log in `raw` as one: the servers' `keys-<server>.log`, and a `keys.log` from
/// before, in name order.
pub(super) fn key_logs(raw: &Path) -> Result<Vec<u8>, PcapError> {
    let mut names: Vec<PathBuf> = std::fs::read_dir(raw)
        .map_err(|source| PcapError::ListKeyLogs {
            path: raw.to_owned(),
            source,
        })?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            name.starts_with("keys") && name.ends_with(".log")
        })
        .collect();
    names.sort();
    let mut keys = Vec::new();
    for path in names {
        let read = std::fs::read(&path).map_err(|source| PcapError::ReadKeyLog {
            path: path.clone(),
            source,
        })?;
        keys.extend_from_slice(&read);
        if keys.last().is_some_and(|&byte| byte != b'\n') {
            keys.push(b'\n');
        }
    }
    Ok(keys)
}

/// Writes `wire`'s [`PCAPNG`] from `raw`'s [`CAPTURE`] and its key logs: how many packets it
/// holds and whether the secrets are in it; nothing, without a capture.
pub(super) fn assemble(raw: &Path, wire: &Path) -> Result<Option<Assembled>, PcapError> {
    let capture = raw.join(CAPTURE);
    let Some(pcap) = read(raw)? else {
        return Ok(None);
    };
    let header = header(&capture, &pcap)?;
    let keys = Some(key_logs(raw)?);
    let path = wire.join(PCAPNG);
    let file = std::fs::File::create(&path).map_err(|source| PcapError::Create {
        path: path.clone(),
        source,
    })?;
    let mut out = Out {
        out: BufWriter::new(file),
        path: &path,
    };
    let packets = write_pcapng(&mut out, &header, &pcap, keys.as_deref())?;
    out.out.flush().map_err(|source| PcapError::Flush {
        path: path.clone(),
        source,
    })?;
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

/// `capture`'s header, `pcap`'s first bytes.
fn header(capture: &Path, pcap: &[u8]) -> Result<Header, PcapError> {
    if pcap.len() < HEADER {
        return Err(PcapError::Short {
            path: capture.to_owned(),
            len: pcap.len(),
        });
    }
    let magic = u32::from_le_bytes([pcap[0], pcap[1], pcap[2], pcap[3]]);
    let (swapped, resolution) = match magic {
        0xA1B2_C3D4 => (false, 6),
        0xD4C3_B2A1 => (true, 6),
        0xA1B2_3C4D => (false, 9),
        0x4D3C_B2A1 => (true, 9),
        magic => {
            return Err(PcapError::Magic {
                path: capture.to_owned(),
                magic,
            });
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

/// Where the pcapng goes, and the file it is, to say.
struct Out<'a, W> {
    out: W,
    path: &'a Path,
}

impl<W: Write> Out<'_, W> {
    /// pcapng's blocks: each a type, its total length, its body padded to 32 bits, and the
    /// total length again, so that it reads backwards too. `what` says which, to say.
    fn block(&mut self, what: &'static str, kind: u32, body: &[u8]) -> Result<(), PcapError> {
        let padding = (4 - body.len() % 4) % 4;
        let len = 12 + body.len() + padding;
        let total =
            u32::try_from(len).map_err(|_| PcapError::BlockTooLarge { block: what, len })?;
        let write = |out: &mut W| -> std::io::Result<()> {
            out.write_all(&kind.to_le_bytes())?;
            out.write_all(&total.to_le_bytes())?;
            out.write_all(body)?;
            out.write_all(&[0; 4][..padding])?;
            out.write_all(&total.to_le_bytes())
        };
        write(&mut self.out).map_err(|source| PcapError::Write {
            path: self.path.to_owned(),
            block: what,
            source,
        })
    }
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

/// Writes the pcap's packets, `header`'s, as pcapng, with `keys`: how many packets it wrote.
fn write_pcapng<W: Write>(
    out: &mut Out<'_, W>,
    header: &Header,
    pcap: &[u8],
    keys: Option<&[u8]>,
) -> Result<usize, PcapError> {
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
    out.block("the section header", SECTION_HEADER, &section)?;

    let mut interface = Vec::new();
    interface.extend_from_slice(&header.link_type.to_le_bytes());
    interface.extend_from_slice(&0u16.to_le_bytes());
    interface.extend_from_slice(&header.snaplen.to_le_bytes());
    option(&mut interface, 2, b"lo");
    option(&mut interface, 9, &[header.resolution]);
    option(&mut interface, END_OF_OPTIONS, &[]);
    out.block("the interface", INTERFACE_DESCRIPTION, &interface)?;

    if let Some(keys) = keys.filter(|keys| !keys.is_empty()) {
        let mut secrets = Vec::new();
        secrets.extend_from_slice(&TLS_KEY_LOG.to_le_bytes());
        secrets.extend_from_slice(
            &u32::try_from(keys.len())
                .map_err(|_| PcapError::KeyLogTooLarge { len: keys.len() })?
                .to_le_bytes(),
        );
        secrets.extend_from_slice(keys);
        out.block("the secrets", DECRYPTION_SECRETS, &secrets)?;
    }

    let packets = packets(header, pcap);
    for &(at, original, bytes) in &packets {
        let mut packet = Vec::with_capacity(20 + bytes.len());
        packet.extend_from_slice(&0u32.to_le_bytes());
        packet.extend_from_slice(&u32::try_from(at >> 32).unwrap_or(u32::MAX).to_le_bytes());
        packet.extend_from_slice(&(at as u32).to_le_bytes());
        packet.extend_from_slice(&u32::try_from(bytes.len()).unwrap_or(u32::MAX).to_le_bytes());
        packet.extend_from_slice(&original.to_le_bytes());
        packet.extend_from_slice(bytes);
        out.block("a packet", ENHANCED_PACKET, &packet)?;
    }
    Ok(packets.len())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        DECRYPTION_SECRETS, ENHANCED_PACKET, INTERFACE_DESCRIPTION, Out, PcapError, SECTION_HEADER,
        header, write_pcapng,
    };

    /// `capture` written as pcapng with `keys`: the pcapng, and how many packets it holds.
    fn pcapng(capture: &[u8], keys: Option<&[u8]>) -> Result<(Vec<u8>, usize), PcapError> {
        let header = header(Path::new("capture.pcap"), capture)?;
        let mut out = Out {
            out: Vec::new(),
            path: Path::new("capture.pcapng"),
        };
        let written = write_pcapng(&mut out, &header, capture, keys)?;
        Ok((out.out, written))
    }

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
        let (out, written) = pcapng(&capture, Some(keys)).expect("a pcapng");
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
        let (out, _) = pcapng(&capture, Some(b"")).expect("a pcapng");
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
        let (out, written) = pcapng(&capture, None).expect("a pcapng");
        assert_eq!(written, 1);
        let blocks = blocks(&out);
        assert!(blocks[1].1.windows(5).any(|w| w == [9, 0, 1, 0, 6]));
        let packet = blocks[2].1;
        assert_eq!(&packet[8..12], &7_000_003u32.to_le_bytes());
    }

    #[test]
    fn not_a_pcap_is_said() {
        let error = pcapng(b"not a pcap at all, not even close", None).expect_err("no pcapng");
        assert!(
            matches!(
                error,
                PcapError::Magic {
                    magic: 0x2074_6f6e,
                    ..
                }
            ),
            "{error}"
        );
    }
}
