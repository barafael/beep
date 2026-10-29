//! HTTP/1.1: requests and their responses in order, and where a `101 Switching Protocols` hands
//! the connection over to WebSocket.

use super::stream::Stream;

/// Request or response headers, names lowercased, in order.
pub(crate) type Headers = Vec<(String, String)>;

pub(crate) fn header<'a>(headers: &'a Headers, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(found, _)| found == name)
        .map(|(_, value)| value.as_str())
}

/// A request, and its response once one came.
#[derive(Debug)]
pub(crate) struct Exchange {
    pub(crate) at: u64,
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) headers: Headers,
    pub(crate) body: Vec<u8>,
    pub(crate) response: Option<Response>,
}

#[derive(Debug)]
pub(crate) struct Response {
    pub(crate) at: u64,
    pub(crate) status: u16,
    pub(crate) headers: Headers,
    pub(crate) body: Vec<u8>,
}

/// Where the connection turned into WebSocket: the offsets on each side after the handshake,
/// and the extensions the server agreed to.
#[derive(Debug)]
pub(crate) struct Upgrade {
    pub(crate) sent: usize,
    pub(crate) received: usize,
    pub(crate) extensions: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Connection {
    pub(crate) exchanges: Vec<Exchange>,
    pub(crate) upgrade: Option<Upgrade>,
    /// What didn't parse, if anything didn't.
    pub(crate) left: Option<String>,
}

pub(crate) fn parse(sent: &Stream, received: &Stream) -> Connection {
    let mut connection = Connection::default();
    let (mut at_sent, mut at_received) = (0, 0);
    while at_sent < sent.bytes.len() {
        let request = match request(&sent.bytes[at_sent..]) {
            Ok(Some(request)) => request,
            Ok(None) => {
                connection.left = Some(format!(
                    "{} bytes of a request cut short",
                    sent.bytes.len() - at_sent
                ));
                return connection;
            }
            Err(error) => {
                connection.left = Some(format!("a request: {error}"));
                return connection;
            }
        };
        let at = sent.at(at_sent);
        at_sent += request.length;
        let head_only = request.method == "HEAD";
        let response = if at_received < received.bytes.len() {
            match response(&received.bytes[at_received..], head_only) {
                Ok(Some(response)) => Some(response),
                Ok(None) => None,
                Err(error) => {
                    connection.left = Some(format!("a response: {error}"));
                    None
                }
            }
        } else {
            None
        };
        let response = response.map(|response| {
            let parsed = Response {
                at: received.at(at_received),
                status: response.status,
                headers: response.headers,
                body: response.body,
            };
            at_received += response.length;
            parsed
        });
        let upgraded = response
            .as_ref()
            .is_some_and(|response| response.status == 101);
        let extensions = response.as_ref().and_then(|response| {
            header(&response.headers, "sec-websocket-extensions").map(str::to_owned)
        });
        connection.exchanges.push(Exchange {
            at,
            method: request.method,
            path: request.path,
            headers: request.headers,
            body: request.body,
            response,
        });
        if upgraded {
            connection.upgrade = Some(Upgrade {
                sent: at_sent,
                received: at_received,
                extensions,
            });
            return connection;
        }
        if connection.left.is_some() {
            return connection;
        }
    }
    connection
}

struct Request {
    method: String,
    path: String,
    headers: Headers,
    body: Vec<u8>,
    length: usize,
}

struct ParsedResponse {
    status: u16,
    headers: Headers,
    body: Vec<u8>,
    length: usize,
}

/// A whole request at the start of `bytes`, `None` if it isn't all there.
fn request(bytes: &[u8]) -> Result<Option<Request>, String> {
    let mut slots = [httparse::EMPTY_HEADER; 96];
    let mut parsed = httparse::Request::new(&mut slots);
    let head = match parsed.parse(bytes).map_err(|error| error.to_string())? {
        httparse::Status::Complete(head) => head,
        httparse::Status::Partial => return Ok(None),
    };
    let method = parsed.method.unwrap_or_default().to_owned();
    let path = parsed.path.unwrap_or_default().to_owned();
    let headers = owned(parsed.headers);
    // A request without a length has no body (RFC 9112 6.3).
    Ok(
        body(bytes, head, &headers, false)?.map(|(body, length)| Request {
            method,
            path,
            headers,
            body,
            length,
        }),
    )
}

/// A whole response at the start of `bytes`, `None` if it isn't all there.
fn response(bytes: &[u8], head_only: bool) -> Result<Option<ParsedResponse>, String> {
    let mut slots = [httparse::EMPTY_HEADER; 96];
    let mut parsed = httparse::Response::new(&mut slots);
    let head = match parsed.parse(bytes).map_err(|error| error.to_string())? {
        httparse::Status::Complete(head) => head,
        httparse::Status::Partial => return Ok(None),
    };
    let status = parsed.code.unwrap_or_default();
    let headers = owned(parsed.headers);
    let bodiless = head_only || status == 101 || status == 204 || status == 304 || status < 200;
    if bodiless {
        return Ok(Some(ParsedResponse {
            status,
            headers,
            body: Vec::new(),
            length: head,
        }));
    }
    Ok(
        body(bytes, head, &headers, true)?.map(|(body, length)| ParsedResponse {
            status,
            headers,
            body,
            length,
        }),
    )
}

fn owned(headers: &[httparse::Header<'_>]) -> Headers {
    headers
        .iter()
        .map(|header| {
            (
                header.name.to_ascii_lowercase(),
                String::from_utf8_lossy(header.value).into_owned(),
            )
        })
        .collect()
}

/// The body after a head of `head` bytes, and the message's whole length; `None` if it isn't
/// all there. A response without a length runs to the connection's end (`to_end`).
fn body(
    bytes: &[u8],
    head: usize,
    headers: &Headers,
    to_end: bool,
) -> Result<Option<(Vec<u8>, usize)>, String> {
    let rest = &bytes[head..];
    if header(headers, "transfer-encoding").is_some_and(|coding| coding.contains("chunked")) {
        return Ok(chunked(rest).map(|(body, length)| (body, head + length)));
    }
    match header(headers, "content-length") {
        Some(length) => {
            let length: usize = length
                .trim()
                .parse()
                .map_err(|_| format!("content-length {length:?}"))?;
            Ok(rest
                .get(..length)
                .map(|body| (body.to_vec(), head + length)))
        }
        None if to_end => Ok(Some((rest.to_vec(), bytes.len()))),
        None => Ok(Some((Vec::new(), head))),
    }
}

/// A chunked body joined, and how long it was on the wire with its trailers.
fn chunked(mut bytes: &[u8]) -> Option<(Vec<u8>, usize)> {
    let total = bytes.len();
    let mut body = Vec::new();
    loop {
        let line = bytes.windows(2).position(|pair| pair == b"\r\n")?;
        let size = std::str::from_utf8(&bytes[..line]).ok()?;
        let size = usize::from_str_radix(size.split(';').next()?.trim(), 16).ok()?;
        bytes = &bytes[line + 2..];
        if size == 0 {
            // Trailers, then the empty line.
            loop {
                let line = bytes.windows(2).position(|pair| pair == b"\r\n")?;
                bytes = &bytes[line + 2..];
                if line == 0 {
                    return Some((body, total - bytes.len()));
                }
            }
        }
        body.extend_from_slice(bytes.get(..size)?);
        bytes = bytes.get(size + 2..)?;
    }
}

#[cfg(test)]
mod tests {
    use super::parse;
    use crate::wire::stream::Stream;

    fn stream(parts: &[(u64, &[u8])]) -> Stream {
        let mut stream = Stream::default();
        for &(at, bytes) in parts {
            stream.push(at, bytes);
        }
        stream
    }

    #[test]
    fn requests_pair_with_responses_until_the_upgrade() {
        let sent = stream(&[
            (
                1,
                b"PUT /v1/a HTTP/1.1\r\nHost: x\r\nContent-Length: 2\r\n\r\nhi",
            ),
            (
                3,
                b"GET /v1/websocket/ HTTP/1.1\r\nUpgrade: websocket\r\n\r\n\x82\x00",
            ),
        ]);
        let received = stream(&[
            (2, b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n"),
            (
                4,
                b"HTTP/1.1 101 Switching Protocols\r\nSec-WebSocket-Extensions: permessage-deflate\r\n\r\n",
            ),
        ]);
        let connection = parse(&sent, &received);
        assert_eq!(connection.left, None);
        let [first, second] = connection.exchanges.as_slice() else {
            panic!("{connection:?}");
        };
        assert_eq!(
            (first.method.as_str(), first.body.as_slice()),
            ("PUT", &b"hi"[..])
        );
        let response = first.response.as_ref().unwrap();
        assert_eq!(
            (response.status, response.body.as_slice(), response.at),
            (200, &b"abc"[..], 2)
        );
        assert_eq!(second.path, "/v1/websocket/");
        let upgrade = connection.upgrade.unwrap();
        assert_eq!(&sent.bytes[upgrade.sent..], b"\x82\x00");
        assert_eq!(upgrade.received, received.bytes.len());
        assert_eq!(upgrade.extensions.as_deref(), Some("permessage-deflate"));
    }
}
