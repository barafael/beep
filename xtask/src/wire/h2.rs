//! HTTP/2 (RFC 9113): each side's frames, its header blocks through HPACK, and what each stream
//! carried: the request, the response, their data and trailers. A stream that `CONNECT`s with
//! `:protocol websocket` (RFC 8441) carries WebSocket frames in its data.

use loona_hpack::Decoder;

use super::h1::{Headers, header};
use super::stream::Stream;

/// What a client sends first.
const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

const DATA: u8 = 0;
const HEADERS: u8 = 1;
const RST_STREAM: u8 = 3;
const SETTINGS: u8 = 4;
const PUSH_PROMISE: u8 = 5;
const PING: u8 = 6;
const GOAWAY: u8 = 7;
const CONTINUATION: u8 = 9;

const END_STREAM: u8 = 0x1;
const ACK: u8 = 0x1;
const END_HEADERS: u8 = 0x4;
const PADDED: u8 = 0x8;
const PRIORITY: u8 = 0x20;

/// One side's view of a stream.
#[derive(Default)]
pub(crate) struct Half {
    /// When its first frame came.
    pub(crate) at: Option<u64>,
    /// Its first header block: the request's, or the response's (past any 1xx).
    pub(crate) headers: Headers,
    pub(crate) trailers: Headers,
    pub(crate) data: Stream,
    pub(crate) ended: Option<u64>,
    pub(crate) reset: Option<u32>,
}

#[derive(Default)]
pub(crate) struct H2Stream {
    pub(crate) id: u32,
    pub(crate) client: Half,
    pub(crate) server: Half,
}

impl H2Stream {
    /// An extended CONNECT for WebSocket.
    pub(crate) fn websocket(&self) -> bool {
        header(&self.client.headers, ":method") == Some("CONNECT")
            && header(&self.client.headers, ":protocol") == Some("websocket")
    }

    pub(crate) fn grpc(&self) -> bool {
        header(&self.client.headers, "content-type")
            .is_some_and(|kind| kind.starts_with("application/grpc"))
    }
}

/// A connection-level frame worth comparing: SETTINGS, PING, GOAWAY.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Control {
    pub(crate) at: u64,
    pub(crate) from_client: bool,
    pub(crate) what: String,
}

#[derive(Default)]
pub(crate) struct Connection {
    pub(crate) streams: Vec<H2Stream>,
    pub(crate) controls: Vec<Control>,
    pub(crate) left: Vec<String>,
}

pub(crate) fn parse(sent: &Stream, received: &Stream) -> Connection {
    let mut connection = Connection::default();
    if !sent.bytes.starts_with(PREFACE) {
        connection.left.push("no HTTP/2 preface".to_owned());
        return connection;
    }
    side(sent, PREFACE.len(), true, &mut connection);
    side(received, 0, false, &mut connection);
    connection.streams.sort_by_key(|stream| stream.id);
    connection.controls.sort_by_key(|control| control.at);
    connection
}

/// One side's frames, into the connection's streams.
fn side(stream: &Stream, start: usize, from_client: bool, connection: &mut Connection) {
    let who = if from_client { "client" } else { "server" };
    let mut decoder = Decoder::new();
    let bytes = &stream.bytes;
    let mut offset = start;
    // A header block across CONTINUATION frames: its stream, its flags, the block so far.
    let mut block: Option<(u32, u8, Vec<u8>, u64)> = None;
    while offset < bytes.len() {
        let Some(head) = bytes.get(offset..offset + 9) else {
            connection.left.push(format!(
                "{who}: {} bytes of a frame head",
                bytes.len() - offset
            ));
            return;
        };
        let length = usize::from(head[0]) << 16 | usize::from(head[1]) << 8 | usize::from(head[2]);
        let (kind, flags) = (head[3], head[4]);
        let id = u32::from_be_bytes([head[5], head[6], head[7], head[8]]) & 0x7fff_ffff;
        let Some(payload) = bytes.get(offset + 9..offset + 9 + length) else {
            connection
                .left
                .push(format!("{who}: a frame of {length} bytes cut short"));
            return;
        };
        let at = stream.at(offset);
        offset += 9 + length;
        match kind {
            DATA => {
                let Some(data) = unpadded(payload, flags) else {
                    connection.left.push(format!("{who}: bad padding"));
                    return;
                };
                let half = half(connection, id, from_client, at);
                half.data.push(at, data);
                if flags & END_STREAM != 0 {
                    half.ended = Some(at);
                }
            }
            HEADERS => {
                let Some(mut fragment) = unpadded(payload, flags) else {
                    connection.left.push(format!("{who}: bad padding"));
                    return;
                };
                if flags & PRIORITY != 0 {
                    fragment = fragment.get(5..).unwrap_or_default();
                }
                block = Some((id, flags, fragment.to_vec(), at));
            }
            CONTINUATION => match &mut block {
                Some((_, block_flags, fragment, _)) => {
                    fragment.extend_from_slice(payload);
                    *block_flags |= flags & END_HEADERS;
                }
                None => {
                    connection
                        .left
                        .push(format!("{who}: CONTINUATION without HEADERS"));
                    return;
                }
            },
            PUSH_PROMISE => {
                // Not from Signal's servers; its block would still change the HPACK table.
                connection.left.push(format!("{who}: PUSH_PROMISE"));
                return;
            }
            SETTINGS => {
                let what = if flags & ACK != 0 {
                    "SETTINGS ack".to_owned()
                } else {
                    let settings: Vec<String> = payload
                        .as_chunks::<6>()
                        .0
                        .iter()
                        .map(|setting| {
                            format!(
                                "{}={}",
                                setting_name(u16::from_be_bytes([setting[0], setting[1]])),
                                u32::from_be_bytes([
                                    setting[2], setting[3], setting[4], setting[5]
                                ])
                            )
                        })
                        .collect();
                    format!("SETTINGS {}", settings.join(" "))
                };
                connection.controls.push(Control {
                    at,
                    from_client,
                    what,
                });
            }
            PING => connection.controls.push(Control {
                at,
                from_client,
                what: if flags & ACK != 0 { "PING ack" } else { "PING" }.to_owned(),
            }),
            GOAWAY => {
                let code = payload.get(4..8).map_or(0, |code| {
                    u32::from_be_bytes([code[0], code[1], code[2], code[3]])
                });
                connection.controls.push(Control {
                    at,
                    from_client,
                    what: format!("GOAWAY {code}"),
                });
            }
            RST_STREAM => {
                let code = payload.get(..4).map_or(0, |code| {
                    u32::from_be_bytes([code[0], code[1], code[2], code[3]])
                });
                half(connection, id, from_client, at).reset = Some(code);
            }
            // WINDOW_UPDATE, PRIORITY and the unknown: flow, not content.
            _ => {}
        }
        if let Some((block_id, block_flags, fragment, block_at)) = block.take() {
            if block_flags & END_HEADERS == 0 {
                block = Some((block_id, block_flags, fragment, block_at));
                continue;
            }
            let headers: Headers = match decoder.decode(&fragment) {
                Ok(headers) => headers
                    .into_iter()
                    .map(|(name, value)| {
                        (
                            String::from_utf8_lossy(&name).into_owned(),
                            String::from_utf8_lossy(&value).into_owned(),
                        )
                    })
                    .collect(),
                Err(error) => {
                    // The table is out of step from here on.
                    connection.left.push(format!("{who}: HPACK {error:?}"));
                    return;
                }
            };
            let half = half(connection, block_id, from_client, block_at);
            let informational =
                header(&headers, ":status").is_some_and(|status| status.starts_with('1'));
            if half.headers.is_empty() && !informational {
                half.headers = headers;
            } else if !informational {
                half.trailers = headers;
            }
            if block_flags & END_STREAM != 0 {
                half.ended = Some(block_at);
            }
        }
    }
}

fn half(connection: &mut Connection, id: u32, from_client: bool, at: u64) -> &mut Half {
    let index = match connection.streams.iter().position(|stream| stream.id == id) {
        Some(index) => index,
        None => {
            connection.streams.push(H2Stream {
                id,
                ..H2Stream::default()
            });
            connection.streams.len() - 1
        }
    };
    let stream = &mut connection.streams[index];
    let half = if from_client {
        &mut stream.client
    } else {
        &mut stream.server
    };
    half.at.get_or_insert(at);
    half
}

fn unpadded(payload: &[u8], flags: u8) -> Option<&[u8]> {
    if flags & PADDED == 0 {
        return Some(payload);
    }
    let (&padding, rest) = payload.split_first()?;
    rest.get(..rest.len().checked_sub(usize::from(padding))?)
}

fn setting_name(id: u16) -> String {
    match id {
        1 => "HEADER_TABLE_SIZE".to_owned(),
        2 => "ENABLE_PUSH".to_owned(),
        3 => "MAX_CONCURRENT_STREAMS".to_owned(),
        4 => "INITIAL_WINDOW_SIZE".to_owned(),
        5 => "MAX_FRAME_SIZE".to_owned(),
        6 => "MAX_HEADER_LIST_SIZE".to_owned(),
        8 => "ENABLE_CONNECT_PROTOCOL".to_owned(),
        9 => "NO_RFC7540_PRIORITIES".to_owned(),
        other => format!("0x{other:x}"),
    }
}

/// gRPC's length-prefixed messages in a stream's data: each one's bytes, and whether it says it
/// is compressed.
pub(crate) fn grpc_messages(data: &[u8]) -> (Vec<(bool, Vec<u8>)>, Option<String>) {
    let mut messages = Vec::new();
    let mut rest = data;
    while !rest.is_empty() {
        let Some(head) = rest.get(..5) else {
            return (messages, Some("a gRPC message head cut short".to_owned()));
        };
        let length = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
        let Some(message) = rest.get(5..5 + length) else {
            return (messages, Some("a gRPC message cut short".to_owned()));
        };
        messages.push((head[0] != 0, message.to_vec()));
        rest = &rest[5 + length..];
    }
    (messages, None)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{PREFACE, grpc_messages, parse};
    use crate::wire::h1::header;
    use crate::wire::stream::Stream;

    pub(crate) fn frame(kind: u8, flags: u8, id: u32, payload: &[u8]) -> Vec<u8> {
        let length = payload.len();
        let mut frame = vec![
            (length >> 16) as u8,
            (length >> 8) as u8,
            length as u8,
            kind,
            flags,
        ];
        frame.extend(id.to_be_bytes());
        frame.extend_from_slice(payload);
        frame
    }

    pub(crate) fn headers_block(headers: &[(&str, &str)]) -> Vec<u8> {
        let mut encoder = loona_hpack::Encoder::new();
        encoder.encode(
            headers
                .iter()
                .map(|(name, value)| (name.as_bytes(), value.as_bytes())),
        )
    }

    #[test]
    fn a_websocket_connect_and_a_grpc_call_come_apart() {
        let mut client = PREFACE.to_vec();
        client.extend(frame(4, 0, 0, &[0, 8, 0, 0, 0, 1]));
        client.extend(frame(
            1,
            0x4,
            1,
            &headers_block(&[
                (":method", "CONNECT"),
                (":protocol", "websocket"),
                (":path", "/v1/websocket/"),
                ("user-agent", "Signal-Desktop/7.0 Linux"),
            ]),
        ));
        client.extend(frame(0, 0, 1, b"\x82\x80\x00\x00\x00\x00"));
        client.extend(frame(
            1,
            0x4,
            3,
            &headers_block(&[
                (":method", "POST"),
                (":path", "/org.signal.chat.keys.Keys/GetPreKeyCount"),
                ("content-type", "application/grpc"),
            ]),
        ));
        client.extend(frame(0, 0x1, 3, &[0, 0, 0, 0, 2, 0x08, 0x01]));
        let mut server = frame(4, 0, 0, &[0, 8, 0, 0, 0, 1]);
        server.extend(frame(1, 0x4, 1, &headers_block(&[(":status", "200")])));
        server.extend(frame(1, 0x4, 3, &headers_block(&[(":status", "200")])));
        server.extend(frame(1, 0x5, 3, &headers_block(&[("grpc-status", "0")])));
        let mut sent = Stream::default();
        sent.push(1, &client);
        let mut received = Stream::default();
        received.push(2, &server);
        let connection = parse(&sent, &received);
        assert!(connection.left.is_empty(), "{:?}", connection.left);
        let [ws, grpc] = connection.streams.as_slice() else {
            panic!("{} streams", connection.streams.len());
        };
        assert!(ws.websocket() && !ws.grpc());
        assert_eq!(ws.client.data.bytes, b"\x82\x80\x00\x00\x00\x00");
        assert_eq!(header(&ws.server.headers, ":status"), Some("200"));
        assert!(grpc.grpc());
        assert_eq!(header(&grpc.server.trailers, "grpc-status"), Some("0"));
        assert_eq!(
            grpc_messages(&grpc.client.data.bytes).0,
            [(false, vec![0x08, 0x01])]
        );
        assert_eq!(
            connection.controls[0].what,
            "SETTINGS ENABLE_CONNECT_PROTOCOL=1"
        );
    }
}
