//! WebSocket frames (RFC 6455) one way: unmasked, fragments joined into messages, and inflated
//! where permessage-deflate (RFC 7692) compressed them.

use flate2::{Decompress, FlushDecompress, Status};

use super::stream::Stream;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) enum Opcode {
    Text,
    Binary,
    Close,
    Ping,
    Pong,
    Other(u8),
}

impl Opcode {
    fn from_byte(byte: u8) -> Self {
        match byte {
            1 => Self::Text,
            2 => Self::Binary,
            8 => Self::Close,
            9 => Self::Ping,
            10 => Self::Pong,
            other => Self::Other(other),
        }
    }
}

/// A whole message, or a control frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Message {
    /// When its first frame began.
    pub(crate) at: u64,
    pub(crate) opcode: Opcode,
    pub(crate) payload: Vec<u8>,
    /// Its size on the wire, compressed if it was.
    pub(crate) wire_size: usize,
}

/// What the handshake agreed for one direction: whether messages may be compressed, and whether
/// each starts its compression afresh.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Deflate {
    pub(crate) on: bool,
    pub(crate) no_context_takeover: bool,
}

impl Deflate {
    /// From the server's `Sec-WebSocket-Extensions`, for the client's direction (`client`) or
    /// the server's.
    pub(crate) fn agreed(extensions: Option<&str>, client: bool) -> Self {
        let Some(extensions) = extensions else {
            return Self::default();
        };
        let Some(deflate) = extensions
            .split(',')
            .map(str::trim)
            .find(|extension| extension.starts_with("permessage-deflate"))
        else {
            return Self::default();
        };
        let takeover = if client {
            "client_no_context_takeover"
        } else {
            "server_no_context_takeover"
        };
        Self {
            on: true,
            no_context_takeover: deflate
                .split(';')
                .any(|parameter| parameter.trim() == takeover),
        }
    }
}

/// The messages in `stream` from `start`, and what didn't parse at its end, if anything did not.
pub(crate) fn messages(
    stream: &Stream,
    start: usize,
    deflate: Deflate,
) -> (Vec<Message>, Option<String>) {
    let mut out = Vec::new();
    let mut offset = start;
    let mut inflater = Decompress::new(false);
    // The message being joined from fragments: when, its opcode, compressed, its payload.
    let mut joining: Option<(u64, Opcode, bool, Vec<u8>, usize)> = None;
    let bytes = &stream.bytes;
    while offset < bytes.len() {
        let Some(frame) = Frame::parse(&bytes[offset..]) else {
            return (
                out,
                Some(format!(
                    "{} bytes of a frame cut short",
                    bytes.len() - offset
                )),
            );
        };
        let at = stream.at(offset);
        offset += frame.length;
        if frame.opcode >= 8 {
            out.push(Message {
                at,
                opcode: Opcode::from_byte(frame.opcode),
                payload: frame.payload,
                wire_size: frame.length,
            });
            continue;
        }
        let (started, opcode, compressed, mut payload, wire) = match (joining.take(), frame.opcode)
        {
            (None, 0) => return (out, Some("a continuation without a message".to_owned())),
            (Some(joined), 0) => joined,
            (_, opcode) => (at, Opcode::from_byte(opcode), frame.rsv1, Vec::new(), 0),
        };
        payload.extend_from_slice(&frame.payload);
        let wire = wire + frame.length;
        if !frame.fin {
            joining = Some((started, opcode, compressed, payload, wire));
            continue;
        }
        let payload = if compressed && deflate.on {
            if deflate.no_context_takeover {
                inflater = Decompress::new(false);
            }
            match inflate(&mut inflater, &payload) {
                Some(inflated) => inflated,
                None => return (out, Some("a message that doesn't inflate".to_owned())),
            }
        } else {
            payload
        };
        out.push(Message {
            at: started,
            opcode,
            payload,
            wire_size: wire,
        });
    }
    (
        out,
        joining.map(|_| "a message without its last frame".to_owned()),
    )
}

/// A compressed message's payload inflated: RFC 7692 drops the empty block's tail it ends with.
fn inflate(inflater: &mut Decompress, payload: &[u8]) -> Option<Vec<u8>> {
    let mut input = payload.to_vec();
    input.extend_from_slice(&[0, 0, 0xff, 0xff]);
    let mut out = Vec::new();
    let mut consumed = 0;
    loop {
        out.reserve(64 * 1024);
        let (before_in, before_out) = (inflater.total_in(), inflater.total_out());
        let status = inflater
            .decompress_vec(&input[consumed..], &mut out, FlushDecompress::Sync)
            .ok()?;
        consumed += usize::try_from(inflater.total_in() - before_in).ok()?;
        let progressed = inflater.total_in() != before_in || inflater.total_out() != before_out;
        // All of it read, with room left over: nothing more comes out.
        if status == Status::StreamEnd || (consumed >= input.len() && out.len() < out.capacity()) {
            return Some(out);
        }
        if !progressed {
            return None;
        }
    }
}

struct Frame {
    fin: bool,
    rsv1: bool,
    opcode: u8,
    payload: Vec<u8>,
    /// The frame's whole length, its head too.
    length: usize,
}

impl Frame {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let (&first, rest) = bytes.split_first()?;
        let (&second, mut rest) = rest.split_first()?;
        let masked = second & 0x80 != 0;
        let mut length = u64::from(second & 0x7f);
        let mut head = 2;
        if length == 126 {
            length = u64::from(u16::from_be_bytes(rest.get(..2)?.try_into().ok()?));
            rest = &rest[2..];
            head += 2;
        } else if length == 127 {
            length = u64::from_be_bytes(rest.get(..8)?.try_into().ok()?);
            rest = &rest[8..];
            head += 8;
        }
        let mask = if masked {
            let mask: [u8; 4] = rest.get(..4)?.try_into().ok()?;
            rest = &rest[4..];
            head += 4;
            Some(mask)
        } else {
            None
        };
        let length = usize::try_from(length).ok()?;
        let mut payload = rest.get(..length)?.to_vec();
        if let Some(mask) = mask {
            for (i, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[i % 4];
            }
        }
        Some(Self {
            fin: first & 0x80 != 0,
            rsv1: first & 0x40 != 0,
            opcode: first & 0x0f,
            payload,
            length: head + length,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{Deflate, Opcode, messages};
    use crate::wire::stream::Stream;

    /// A frame as a client sends it: masked.
    pub(crate) fn masked(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [1, 2, 3, 4];
        let mut frame = vec![if fin { 0x80 } else { 0 } | opcode];
        match payload.len() {
            length @ 0..=125 => frame.push(0x80 | length as u8),
            length => {
                frame.push(0x80 | 126);
                frame.extend((length as u16).to_be_bytes());
            }
        }
        frame.extend(mask);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(i, byte)| byte ^ mask[i % 4]),
        );
        frame
    }

    /// A frame as a server sends it.
    pub(crate) fn plain(opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0x80 | opcode, payload.len() as u8];
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn fragments_join_and_controls_stand_alone() {
        let mut stream = Stream::default();
        stream.push(1, &masked(false, 2, b"hel"));
        stream.push(2, &masked(true, 9, b"p"));
        stream.push(3, &masked(true, 0, b"lo"));
        let (found, left) = messages(&stream, 0, Deflate::default());
        assert_eq!(left, None);
        assert_eq!(found.len(), 2);
        assert_eq!(
            (found[0].opcode, found[0].payload.as_slice()),
            (Opcode::Ping, &b"p"[..])
        );
        assert_eq!(
            (found[1].opcode, found[1].payload.as_slice(), found[1].at),
            (Opcode::Binary, &b"hello"[..], 1)
        );
    }

    #[test]
    fn a_compressed_message_inflates() {
        use flate2::Compression;
        use flate2::write::DeflateEncoder;
        use std::io::Write;
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"compressed hello").unwrap();
        // A sync flush, as a sender ends each message: in an empty stored block, which RFC 7692
        // leaves off.
        encoder.flush().unwrap();
        let mut compressed = encoder.get_ref().clone();
        assert!(compressed.ends_with(&[0, 0, 0xff, 0xff]));
        compressed.truncate(compressed.len() - 4);
        let mut frame = plain(2, &compressed);
        frame[0] |= 0x40;
        let mut stream = Stream::default();
        stream.push(1, &frame);
        let deflate = Deflate::agreed(
            Some("permessage-deflate; server_no_context_takeover"),
            false,
        );
        let (found, left) = messages(&stream, 0, deflate);
        assert_eq!(left, None);
        assert_eq!(found[0].payload, b"compressed hello");
    }
}
