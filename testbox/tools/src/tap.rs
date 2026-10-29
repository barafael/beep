//! What the `tap` tool writes for each connection it relays, and reads back: the plaintext each
//! way, in the order it came, and what the connection was.
//!
//! A connection's bytes are in `<n>-<port>.tap`, records one after the other: when (nanoseconds
//! since the epoch, u64 little-endian), which way ([`Direction`], one byte), how many bytes (u32
//! little-endian), the bytes. [`Direction::Closed`] records have none: that side closed. Its
//! [`Connection`] is in `<n>-<port>.json`.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    /// From the client to the server.
    Sent,
    /// From the server to the client.
    Received,
    /// The client closed its side.
    ClientClosed,
    /// The server closed its side.
    ServerClosed,
}

impl Direction {
    fn byte(self) -> u8 {
        match self {
            Self::Sent => 0,
            Self::Received => 1,
            Self::ClientClosed => 2,
            Self::ServerClosed => 3,
        }
    }

    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Sent),
            1 => Some(Self::Received),
            2 => Some(Self::ClientClosed),
            3 => Some(Self::ServerClosed),
            _ => None,
        }
    }
}

/// A connection the tap relayed: where it came from and went, and its TLS as each side had it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    /// Numbers the connections in the order they came.
    pub n: u64,
    /// The port the client meant ([`crate::keys::TAPPED_PORTS`]).
    pub port: u16,
    /// The client's address and port, as the tap saw it.
    pub peer: String,
    /// When it came, in nanoseconds since the epoch.
    pub opened: u64,
    /// What the client's hello asked for.
    pub hello: Hello,
    /// The protocol the server chose (ALPN), which the client got too.
    pub alpn: Option<String>,
    /// The TLS version and cipher suite the server and the tap agreed on.
    pub upstream_version: Option<String>,
    pub upstream_suite: Option<String>,
    /// Why the tap gave up on it before relaying, if it did.
    pub failed: Option<String>,
}

/// A client's TLS hello: what tells the clients' TLS stacks apart.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub server_name: Option<String>,
    pub alpn: Vec<String>,
    pub cipher_suites: Vec<String>,
    pub signature_schemes: Vec<String>,
}

/// One record of a connection's bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub at: u64,
    pub direction: Direction,
    pub bytes: Vec<u8>,
}

impl Record {
    pub fn write_to(&self, out: &mut impl Write) -> io::Result<()> {
        let length = u32::try_from(self.bytes.len())
            .map_err(|_too_long| io::Error::new(io::ErrorKind::InvalidInput, "a record > 4 GiB"))?;
        out.write_all(&self.at.to_le_bytes())?;
        out.write_all(&[self.direction.byte()])?;
        out.write_all(&length.to_le_bytes())?;
        out.write_all(&self.bytes)
    }

    /// The next record, `None` at the end. A record cut short, as when the box stopped while
    /// the tap wrote, ends the file too.
    pub fn read_from(input: &mut impl Read) -> io::Result<Option<Self>> {
        let mut head = [0; 13];
        match input.read_exact(&mut head) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(error),
        }
        let (at, rest) = head.split_at(8);
        let (direction, length) = rest.split_at(1);
        let at = u64::from_le_bytes(at.try_into().expect("8 bytes"));
        let direction = Direction::from_byte(direction[0]).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("no direction {}", direction[0]),
            )
        })?;
        let length = u32::from_le_bytes(length.try_into().expect("4 bytes"));
        let mut bytes = vec![0; length as usize];
        match input.read_exact(&mut bytes) {
            Ok(()) => Ok(Some(Self {
                at,
                direction,
                bytes,
            })),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// The file names of connection `n` to `port`: its bytes', its [`Connection`]'s.
pub fn file_names(n: u64, port: u16) -> (String, String) {
    (format!("{n:05}-{port}.tap"), format!("{n:05}-{port}.json"))
}

#[cfg(test)]
mod tests {
    use super::{Direction, Record};

    #[test]
    fn records_read_back_as_written() {
        let records = [
            Record {
                at: 1,
                direction: Direction::Sent,
                bytes: b"GET / HTTP/1.1\r\n\r\n".to_vec(),
            },
            Record {
                at: 2,
                direction: Direction::Received,
                bytes: vec![0; 70_000],
            },
            Record {
                at: 3,
                direction: Direction::ClientClosed,
                bytes: Vec::new(),
            },
        ];
        let mut file = Vec::new();
        for record in &records {
            record.write_to(&mut file).unwrap();
        }
        let mut input = file.as_slice();
        let read: Vec<Record> =
            std::iter::from_fn(|| Record::read_from(&mut input).unwrap()).collect();
        assert_eq!(read, records);
    }

    #[test]
    fn a_record_cut_short_ends_the_file() {
        let mut file = Vec::new();
        Record {
            at: 1,
            direction: Direction::Sent,
            bytes: b"whole".to_vec(),
        }
        .write_to(&mut file)
        .unwrap();
        Record {
            at: 2,
            direction: Direction::Sent,
            bytes: b"cut short".to_vec(),
        }
        .write_to(&mut file)
        .unwrap();
        file.truncate(file.len() - 3);
        let mut input = file.as_slice();
        assert!(Record::read_from(&mut input).unwrap().is_some());
        assert!(Record::read_from(&mut input).unwrap().is_none());
    }
}
