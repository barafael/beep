//! One of the box's TLS connections as decoded from `--capture`'s packet capture and the servers'
//! key logs (xtask/src/wire): the plaintext each way, in the order it came, and what the
//! connection was. The form the relay `--capture` used to put in the pod wrote, kept: a record
//! from before decodes the same way.
//!
//! A connection's bytes go in `<n>-<port>.tap`, records one after the other: when (nanoseconds
//! since the epoch, u64 little-endian), which way ([`Direction`], one byte), how many bytes (u32
//! little-endian), the bytes. The closing [`Direction`]s' records have none: that side closed.
//! Its [`Connection`] goes in `<n>-<port>.json`.

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

/// A connection: where it came from and went, and its TLS as each side had it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    /// Numbers the connections in the order they came.
    pub n: u64,
    /// The server's port ([`crate::keys::CAPTURED_PORTS`]).
    pub port: u16,
    /// The client's address and port.
    pub peer: String,
    /// When it came, in nanoseconds since the epoch.
    pub opened: u64,
    /// What the client's hello asked for.
    pub hello: Hello,
    /// The protocol the server chose (ALPN), which the client got too.
    pub alpn: Option<String>,
    /// The TLS version and cipher suite the server chose.
    pub upstream_version: Option<String>,
    pub upstream_suite: Option<String>,
    /// Why there is no plaintext of it, if there is none: a handshake that didn't finish, a
    /// session without a key.
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

/// A record of a connection's bytes that couldn't be written or read.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    /// More bytes than its length's four bytes count.
    #[error("a record of {length} bytes, more than 4 GiB")]
    TooLong { length: usize },
    #[error("writing a record")]
    Write(#[source] io::Error),
    #[error("reading a record")]
    Read(#[source] io::Error),
    /// A direction byte that names none.
    #[error("a record with no direction {byte}")]
    Direction { byte: u8 },
}

/// One record of a connection's bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Record {
    pub at: u64,
    pub direction: Direction,
    pub bytes: Vec<u8>,
}

impl Record {
    pub fn write_to(&self, out: &mut impl Write) -> Result<(), RecordError> {
        let length = u32::try_from(self.bytes.len()).map_err(|_too_long| RecordError::TooLong {
            length: self.bytes.len(),
        })?;
        out.write_all(&self.at.to_le_bytes())
            .and_then(|()| out.write_all(&[self.direction.byte()]))
            .and_then(|()| out.write_all(&length.to_le_bytes()))
            .and_then(|()| out.write_all(&self.bytes))
            .map_err(RecordError::Write)
    }

    /// The next record, `None` at the end. A record cut short ends the file too.
    pub fn read_from(input: &mut impl Read) -> Result<Option<Self>, RecordError> {
        let mut head = [0; 13];
        match input.read_exact(&mut head) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(error) => return Err(RecordError::Read(error)),
        }
        let (at, rest) = head.split_at(8);
        let (direction, length) = rest.split_at(1);
        let at = u64::from_le_bytes(at.try_into().expect("8 bytes"));
        let direction = Direction::from_byte(direction[0])
            .ok_or(RecordError::Direction { byte: direction[0] })?;
        let length = u32::from_le_bytes(length.try_into().expect("4 bytes"));
        let mut bytes = vec![0; length as usize];
        match input.read_exact(&mut bytes) {
            Ok(()) => Ok(Some(Self {
                at,
                direction,
                bytes,
            })),
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
            Err(error) => Err(RecordError::Read(error)),
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
